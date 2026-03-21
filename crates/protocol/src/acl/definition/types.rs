//! Core ACL type definitions - tags, permission bits, and entries.
//!
//! These types provide a POSIX-oriented view of ACL data. Each entry
//! combines a tag identifying the entity with the granted permission bits.
//!
//! # Upstream Reference
//!
//! Maps to `SMB_ACL_*` tag constants and `rsync_ace` struct in
//! upstream rsync's `acls.c` lines 37-60.

/// POSIX ACL tag identifying the type and scope of an ACL entry.
///
/// Each entry in a POSIX ACL has a tag that determines what entity the
/// permission bits apply to. The standard entries (`UserObj`, `GroupObj`,
/// `Other`, `Mask`) appear at most once per ACL. Named entries (`User`,
/// `Group`) can appear multiple times with distinct qualifier IDs.
///
/// # Upstream Reference
///
/// Maps to `SMB_ACL_USER`, `SMB_ACL_GROUP`, `SMB_ACL_USER_OBJ`,
/// `SMB_ACL_GROUP_OBJ`, `SMB_ACL_OTHER`, `SMB_ACL_MASK` in upstream
/// rsync's `acls.c`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AclTag {
    /// File owner permissions.
    UserObj,
    /// Owning group permissions.
    GroupObj,
    /// World (other) permissions.
    Other,
    /// ACL mask - limits effective permissions for named entries and group.
    Mask,
    /// Named user entry with a UID qualifier.
    User(u32),
    /// Named group entry with a GID qualifier.
    Group(u32),
}

/// Permission bits for a single ACL entry.
///
/// Standard POSIX ACL permission values: read (4), write (2), execute (1).
/// Stored as a `u8` with only the lower 3 bits meaningful.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct AclPerms(u8);

/// Read permission bit (4).
const PERM_READ: u8 = 4;
/// Write permission bit (2).
const PERM_WRITE: u8 = 2;
/// Execute permission bit (1).
const PERM_EXECUTE: u8 = 1;

impl AclPerms {
    /// Creates permission bits from a raw value, masking to valid bits.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0x07)
    }

    /// Returns the raw permission bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns true if the read bit is set.
    #[must_use]
    pub const fn read(self) -> bool {
        self.0 & PERM_READ != 0
    }

    /// Returns true if the write bit is set.
    #[must_use]
    pub const fn write(self) -> bool {
        self.0 & PERM_WRITE != 0
    }

    /// Returns true if the execute bit is set.
    #[must_use]
    pub const fn execute(self) -> bool {
        self.0 & PERM_EXECUTE != 0
    }
}

impl std::fmt::Display for AclPerms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}",
            if self.read() { 'r' } else { '-' },
            if self.write() { 'w' } else { '-' },
            if self.execute() { 'x' } else { '-' },
        )
    }
}

/// A single entry in a POSIX ACL - a tag plus permission bits.
///
/// Represents one line in the output of `getfacl(1)`, combining the
/// entity qualifier (tag) with the granted permissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AclEntry {
    /// The tag identifying what entity this entry applies to.
    pub tag: AclTag,
    /// The permission bits granted to that entity.
    pub perms: AclPerms,
}

impl AclEntry {
    /// Creates a new ACL entry.
    #[must_use]
    pub const fn new(tag: AclTag, perms: AclPerms) -> Self {
        Self { tag, perms }
    }
}
