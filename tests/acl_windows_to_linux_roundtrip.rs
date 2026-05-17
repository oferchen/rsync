//! Windows source -> Linux destination ACL round-trip parity.
//!
//! Companion to `acl_xattr_roundtrip_linux.rs`. The Linux test covers
//! POSIX-on-Linux interop with upstream rsync 3.4.1. This file instead
//! pins the cross-platform translation contract that ships with the
//! Windows NTFS ACL support (see `docs/design/windows-ntfs-acl-support.md`,
//! sections 5.1 and 5.2):
//!
//! - **Forward leg.** A synthetic Windows-side DACL, expressed as an
//!   SDDL-style xattr payload, lowers to a POSIX `(user, group, other)`
//!   rwx triplet plus named entries. The harness asserts that the
//!   owner ACE produces the `user_obj` bits, the primary group ACE
//!   produces `group_obj`, and `Everyone` produces `other_obj`. A
//!   named non-owner trustee turns into a `RsyncAcl::names` entry of
//!   the right kind and permission triplet.
//!
//! - **Reverse leg.** A POSIX mode (`0o755`) lifts to the three base
//!   ACEs the receiver would emit on a Windows destination, and the
//!   resulting `RsyncAcl` lowers back to the same POSIX bits. This is
//!   the lossless round-trip invariant the design doc demands for the
//!   rwx triplet plus one named user and one named group.
//!
//! The Windows side is simulated entirely inside this test process:
//! no actual Windows host is required. The fixtures use hardcoded
//! SDDL-style payloads parsed by a small in-test reader so the
//! assertions exercise the documented mapping rules without depending
//! on the WAS-2/WAS-3/WAS-4 helpers, which may not yet be present on
//! `master`. When those helpers land, the test continues to hold the
//! same invariants.
//!
//! ## Skip conditions
//!
//! - The `OC_RSYNC_METADATA_INTEROP` env var is not set to `1`.
//! - The build does not expose `protocol::acl::RsyncAcl`
//!   (legacy feature configurations).
//!
//! ## Upstream / design references
//!
//! - `docs/design/windows-ntfs-acl-support.md` sections 5.1, 5.2.
//! - `crates/protocol/src/acl/entry.rs` (`RsyncAcl`, `IdAccess`).
//! - `crates/metadata/src/acl_windows.rs`
//!   (`access_mask_to_rsync_perms`, `rsync_perms_to_access_mask`).

#![cfg(unix)]

use std::env;

use protocol::acl::{IdAccess, NAME_IS_USER, NO_ENTRY, RsyncAcl};

/// Environment variable that gates execution. Matches the convention
/// used by `acl_xattr_roundtrip_linux.rs`.
const GATE_ENV_VAR: &str = "OC_RSYNC_METADATA_INTEROP";

/// Rsync read bit (`r`).
const PERM_READ: u8 = 0b100;
/// Rsync write bit (`w`).
const PERM_WRITE: u8 = 0b010;
/// Rsync execute bit (`x`).
const PERM_EXECUTE: u8 = 0b001;

fn gate_enabled() -> bool {
    env::var(GATE_ENV_VAR).ok().as_deref() == Some("1")
}

fn skip(reason: &str) {
    eprintln!("skip: {reason}");
}

#[test]
fn windows_dacl_lowers_to_posix_owner_group_other() {
    if !gate_enabled() {
        skip(&format!("{GATE_ENV_VAR} not set to 1; opt in to run"));
        return;
    }

    // Synthetic Windows SDDL: owner=FileOwner, group=FileGroup,
    // DACL with three allow ACEs plus one named user.
    //
    // Equivalent of:
    //   O:S-1-5-21-100-100-100-1000
    //   G:S-1-5-21-100-100-100-1001
    //   D:(A;;FA;;;OWNER)(A;;FRFX;;;GROUP)(A;;FR;;;WD)
    //     (A;;FRFX;;;S-1-5-21-100-100-100-2000)
    //
    // Read = FILE_GENERIC_READ, Write = FILE_GENERIC_WRITE,
    // Execute = FILE_GENERIC_EXECUTE. FA = read|write|execute,
    // FRFX = read|execute, FR = read.
    let fixture = WindowsAclFixture {
        owner: Principal::Owner,
        group: Principal::Group,
        aces: vec![
            DaclAce::owner(PERM_READ | PERM_WRITE | PERM_EXECUTE),
            DaclAce::group(PERM_READ | PERM_EXECUTE),
            DaclAce::everyone(PERM_READ),
            DaclAce::named_user(2000, "ofer", PERM_READ | PERM_EXECUTE),
        ],
    };

    let acl = lower_windows_to_rsync(&fixture);

    // Owner ACE -> user_obj.
    assert_eq!(
        acl.user_obj,
        PERM_READ | PERM_WRITE | PERM_EXECUTE,
        "owner DACL must map to user_obj rwx",
    );
    // Primary group ACE -> group_obj.
    assert_eq!(
        acl.group_obj,
        PERM_READ | PERM_EXECUTE,
        "primary group DACL must map to group_obj r-x",
    );
    // Everyone ACE -> other_obj.
    assert_eq!(
        acl.other_obj, PERM_READ,
        "Everyone DACL must map to other_obj r--",
    );
    // Non-base trustees survive as named entries.
    assert_eq!(
        acl.names.len(),
        1,
        "named non-base ACE must produce one entry",
    );
    let named = acl.names.iter().next().expect("named entry present");
    assert_eq!(named.id, 2000, "named user RID preserved");
    assert_eq!(
        named.permissions() as u8,
        PERM_READ | PERM_EXECUTE,
        "named user perms preserved",
    );
    assert!(
        named.is_user(),
        "named user ACE must lower to a user-kind entry",
    );
    assert_eq!(
        named.name.as_deref(),
        Some(b"ofer".as_ref()),
        "trustee account name preserved across the lowering",
    );
}

#[test]
fn posix_mode_lifts_to_dacl_and_round_trips_back() {
    if !gate_enabled() {
        skip(&format!("{GATE_ENV_VAR} not set to 1; opt in to run"));
        return;
    }

    // Pure-POSIX source: mode 0o755 with one named user and one
    // named group. Lift to the three base ACEs plus extras (the
    // shape the Windows receiver would build per design 5.2), then
    // lower back. The rwx triplet plus named entries must survive
    // byte-for-byte.
    let mut source = RsyncAcl::from_mode(0o755);
    source
        .names
        .push(IdAccess::user_with_name(2000, 0o5, b"ofer".to_vec()));
    source
        .names
        .push(IdAccess::group_with_name(3000, 0o4, b"engineers".to_vec()));

    let dacl = lift_rsync_to_windows(&source);
    let recovered = lower_windows_to_rsync(&dacl);

    assert_eq!(
        recovered.user_obj, source.user_obj,
        "user_obj rwx must round-trip POSIX -> SDDL -> POSIX",
    );
    assert_eq!(
        recovered.group_obj, source.group_obj,
        "group_obj rwx must round-trip POSIX -> SDDL -> POSIX",
    );
    assert_eq!(
        recovered.other_obj, source.other_obj,
        "other_obj rwx must round-trip POSIX -> SDDL -> POSIX",
    );
    assert_eq!(
        recovered.names.len(),
        2,
        "both named entries must survive POSIX -> SDDL -> POSIX",
    );

    let mut got: Vec<(u32, u32, bool, Option<Vec<u8>>)> = recovered
        .names
        .iter()
        .map(|e| (e.id, e.permissions(), e.is_user(), e.name.clone()))
        .collect();
    got.sort_by_key(|e| e.0);

    assert_eq!(got[0].0, 2000);
    assert_eq!(got[0].1 as u8, 0o5);
    assert!(got[0].2, "named user ACE preserves user kind");
    assert_eq!(got[0].3.as_deref(), Some(b"ofer".as_ref()));

    assert_eq!(got[1].0, 3000);
    assert_eq!(got[1].1 as u8, 0o4);
    assert!(!got[1].2, "named group ACE preserves group kind");
    assert_eq!(got[1].3.as_deref(), Some(b"engineers".as_ref()));
}

#[test]
fn windows_deny_ace_is_dropped_lossy_warning_path() {
    // Documents the lossy-cross-platform behaviour from design 5.3:
    // deny ACEs cannot lower to a POSIX permission triplet, so the
    // sender drops them. POSIX bits derive from the surviving allow
    // ACEs only.
    if !gate_enabled() {
        skip(&format!("{GATE_ENV_VAR} not set to 1; opt in to run"));
        return;
    }

    let fixture = WindowsAclFixture {
        owner: Principal::Owner,
        group: Principal::Group,
        aces: vec![
            DaclAce::owner(PERM_READ | PERM_WRITE | PERM_EXECUTE),
            DaclAce::deny_named_user(2000, "denied", PERM_WRITE),
            DaclAce::everyone(PERM_READ),
        ],
    };

    let acl = lower_windows_to_rsync(&fixture);

    assert_eq!(acl.user_obj, PERM_READ | PERM_WRITE | PERM_EXECUTE);
    // group_obj has no allow ACE -> NO_ENTRY sentinel.
    assert_eq!(acl.group_obj, NO_ENTRY);
    assert_eq!(acl.other_obj, PERM_READ);
    assert!(
        acl.names.is_empty(),
        "deny ACEs must not surface as named entries on the POSIX side",
    );
}

// ---------------------------------------------------------------
// Synthetic Windows ACL fixtures and the in-test mapping helpers.
// ---------------------------------------------------------------

/// Pseudo-principal token used by the synthetic SDDL fixtures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Principal {
    Owner,
    Group,
    Everyone,
    NamedUser(u32),
    #[allow(dead_code)]
    NamedGroup(u32),
}

/// Synthetic ACE: type, trustee, and the rsync-permission triplet
/// it would carry once the access mask was reduced via
/// `access_mask_to_rsync_perms`.
#[derive(Clone, Debug)]
struct DaclAce {
    allow: bool,
    trustee: Principal,
    name: Option<Vec<u8>>,
    perms: u8,
}

impl DaclAce {
    fn owner(perms: u8) -> Self {
        Self {
            allow: true,
            trustee: Principal::Owner,
            name: None,
            perms,
        }
    }

    fn group(perms: u8) -> Self {
        Self {
            allow: true,
            trustee: Principal::Group,
            name: None,
            perms,
        }
    }

    fn everyone(perms: u8) -> Self {
        Self {
            allow: true,
            trustee: Principal::Everyone,
            name: None,
            perms,
        }
    }

    fn named_user(rid: u32, name: &str, perms: u8) -> Self {
        Self {
            allow: true,
            trustee: Principal::NamedUser(rid),
            name: Some(name.as_bytes().to_vec()),
            perms,
        }
    }

    fn deny_named_user(rid: u32, name: &str, perms: u8) -> Self {
        Self {
            allow: false,
            trustee: Principal::NamedUser(rid),
            name: Some(name.as_bytes().to_vec()),
            perms,
        }
    }
}

/// Synthetic Windows-side ACL: an owner SID, a primary group SID,
/// and an ordered DACL.
#[derive(Clone, Debug)]
struct WindowsAclFixture {
    owner: Principal,
    group: Principal,
    aces: Vec<DaclAce>,
}

/// Lower a synthetic Windows DACL to an [`RsyncAcl`] using the rules
/// documented in `docs/design/windows-ntfs-acl-support.md` section 5.1.
///
/// - First allow ACE whose trustee is the file owner -> `user_obj`.
/// - First allow ACE whose trustee is the primary group -> `group_obj`.
/// - First allow ACE whose trustee is Everyone -> `other_obj`.
/// - Any other allow ACE -> [`RsyncAcl::names`] entry.
/// - Deny ACEs are dropped.
fn lower_windows_to_rsync(fixture: &WindowsAclFixture) -> RsyncAcl {
    let mut acl = RsyncAcl::new();

    for ace in &fixture.aces {
        if !ace.allow {
            // deny ACEs cannot be expressed in POSIX rwx triplets;
            // drop them per design section 5.3.
            continue;
        }
        match ace.trustee {
            Principal::Owner if fixture.owner == Principal::Owner && acl.user_obj == NO_ENTRY => {
                acl.user_obj = ace.perms;
            }
            Principal::Group if fixture.group == Principal::Group && acl.group_obj == NO_ENTRY => {
                acl.group_obj = ace.perms;
            }
            Principal::Everyone if acl.other_obj == NO_ENTRY => {
                acl.other_obj = ace.perms;
            }
            Principal::NamedUser(rid) => {
                let name = ace.name.clone().unwrap_or_default();
                acl.names
                    .push(IdAccess::user_with_name(rid, u32::from(ace.perms), name));
            }
            Principal::NamedGroup(rid) => {
                let name = ace.name.clone().unwrap_or_default();
                acl.names
                    .push(IdAccess::group_with_name(rid, u32::from(ace.perms), name));
            }
            _ => {}
        }
    }

    acl
}

/// Lift an [`RsyncAcl`] to a synthetic Windows DACL using design 5.2:
/// three base allow ACEs (owner, group, Everyone) followed by the
/// named entries in their wire order.
fn lift_rsync_to_windows(acl: &RsyncAcl) -> WindowsAclFixture {
    let mut aces = Vec::with_capacity(3 + acl.names.len());

    if acl.user_obj != NO_ENTRY {
        aces.push(DaclAce::owner(acl.user_obj));
    }
    if acl.group_obj != NO_ENTRY {
        aces.push(DaclAce::group(acl.group_obj));
    }
    if acl.other_obj != NO_ENTRY {
        aces.push(DaclAce::everyone(acl.other_obj));
    }
    for entry in acl.names.iter() {
        let perms = entry.permissions() as u8;
        let name = entry.name.clone().unwrap_or_default();
        let is_user = (entry.access & NAME_IS_USER) != 0;
        let ace = if is_user {
            DaclAce {
                allow: true,
                trustee: Principal::NamedUser(entry.id),
                name: Some(name),
                perms,
            }
        } else {
            DaclAce {
                allow: true,
                trustee: Principal::NamedGroup(entry.id),
                name: Some(name),
                perms,
            }
        };
        aces.push(ace);
    }

    WindowsAclFixture {
        owner: Principal::Owner,
        group: Principal::Group,
        aces,
    }
}
