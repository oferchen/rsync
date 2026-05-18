//! POSIX permission bit <-> DACL mapping for Windows-to-Unix
//! interoperability.
//!
//! Mirrors the lossy conversion rules documented in
//! `docs/design/windows-ntfs-acl-support.md` section 5: a POSIX rwx
//! triplet maps to three canonical allow ACEs and vice-versa.

use super::common::warn_partial_apply;
use super::sddl::{
    SDDL_AUTHENTICATED_USERS, SDDL_EVERYONE, parse_aces, perms_to_sddl_rights,
    sddl_rights_to_perms, split_sddl,
};

/// Converts an SDDL security-descriptor string into a POSIX permission
/// mode triplet (rwxrwxrwx, 9 bits in the canonical 0o000-0o777 range).
///
/// Mapping rules (matching `docs/design/windows-ntfs-acl-support.md`
/// section 5.1):
///
/// - The first allow ACE whose trustee matches the descriptor's owner
///   SID supplies the owner rwx bits.
/// - The first allow ACE whose trustee matches the group SID supplies
///   the group rwx bits.
/// - The first allow ACE addressed to `WD` (Everyone) or, in its
///   absence, `AU` (Authenticated Users) supplies the other rwx bits.
///
/// Deny ACEs, inherited ACEs (`AceFlags` carrying `ID`), and access bits
/// outside `FR`/`FW`/`FX`/`FA` are dropped with a one-time warning to
/// reflect documented lossy behaviour. If a triplet has no matching ACE
/// the corresponding three bits remain `0`.
///
/// # Panics
///
/// Never panics. Malformed input returns `0`.
#[must_use]
pub fn dacl_to_posix_mode(sddl: &str) -> u32 {
    let (owner, group, dacl, _sacl) = split_sddl(sddl);
    let Some(dacl) = dacl else {
        return 0;
    };
    let owner = owner.unwrap_or("");
    let group = group.unwrap_or("");

    let mut owner_perms: u8 = 0;
    let mut group_perms: u8 = 0;
    let mut other_perms: u8 = 0;
    let mut owner_seen = false;
    let mut group_seen = false;
    let mut everyone_seen = false;
    let mut authenticated_perms: u8 = 0;
    let mut authenticated_seen = false;
    let mut dropped = false;

    for ace in parse_aces(dacl) {
        if ace.flags.contains("ID") {
            dropped = true;
            continue;
        }
        if !ace.ace_type.eq_ignore_ascii_case("A") {
            if ace.ace_type.eq_ignore_ascii_case("D") {
                dropped = true;
            }
            continue;
        }
        let perms = sddl_rights_to_perms(ace.rights);
        if perms == 0 {
            continue;
        }
        if !owner.is_empty() && ace.trustee == owner && !owner_seen {
            owner_perms = perms;
            owner_seen = true;
        } else if !group.is_empty() && ace.trustee == group && !group_seen {
            group_perms = perms;
            group_seen = true;
        } else if ace.trustee == SDDL_EVERYONE && !everyone_seen {
            other_perms = perms;
            everyone_seen = true;
        } else if ace.trustee == SDDL_AUTHENTICATED_USERS && !authenticated_seen {
            authenticated_perms = perms;
            authenticated_seen = true;
        }
    }

    if !everyone_seen && authenticated_seen {
        other_perms = authenticated_perms;
    }

    if dropped {
        warn_partial_apply();
    }

    (u32::from(owner_perms) << 6) | (u32::from(group_perms) << 3) | u32::from(other_perms)
}

/// Generates an SDDL security-descriptor string from a POSIX permission
/// mode and the owning user / group SIDs.
///
/// The emitted DACL contains three explicit allow ACEs, in canonical
/// order:
///
/// 1. Allow ACE for the owner SID with the owner triplet's rwx bits.
/// 2. Allow ACE for the group SID with the group triplet's rwx bits.
/// 3. Allow ACE for `WD` (Everyone) with the other triplet's rwx bits.
///
/// Permission triplets with no bits set are still emitted as empty
/// rights ACEs (`(A;;;;;<SID>)`) so the round-trip preserves the
/// distinction between "no permissions" and "ACE omitted entirely".
///
/// The `P` flag is set on the DACL so parent inheritance cannot silently
/// add ACEs that were never on the source, matching section 5.2 of the
/// design document.
///
/// # Panics
///
/// Never panics.
#[must_use]
pub fn posix_mode_to_dacl(mode: u32, owner_sid: &str, group_sid: &str) -> String {
    let owner_perms = ((mode >> 6) & 0o7) as u8;
    let group_perms = ((mode >> 3) & 0o7) as u8;
    let other_perms = (mode & 0o7) as u8;

    format!(
        "O:{owner}G:{group}D:P(A;;{owner_rights};;;{owner})(A;;{group_rights};;;{group})(A;;{other_rights};;;WD)",
        owner = owner_sid,
        group = group_sid,
        owner_rights = perms_to_sddl_rights(owner_perms),
        group_rights = perms_to_sddl_rights(group_perms),
        other_rights = perms_to_sddl_rights(other_perms),
    )
}
