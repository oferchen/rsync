//! ACL data structures for wire protocol.
//!
//! These structures mirror upstream rsync's ACL representation from `acls.c`.

use super::constants::NO_ENTRY;

/// A single named user or group ACL entry.
///
/// Represents one entry in the `ida_entries` list, containing an ID
/// (UID or GID) and access permission bits.
///
/// # Wire Format
///
/// Each entry is encoded as:
/// ```text
/// id         : varint      // UID or GID
/// access     : varint      // (perms << 2) | flags
/// [name_len] : byte        // Only if XFLAG_NAME_FOLLOWS set
/// [name]     : bytes       // Only if XFLAG_NAME_FOLLOWS set
/// ```
///
/// # Upstream Reference
///
/// Corresponds to `id_access` struct in `acls.c` lines 57-60.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IdAccess {
    /// User ID or group ID for this entry.
    pub id: u32,
    /// Access permission bits (rwx) with optional `NAME_IS_USER` flag.
    ///
    /// Lower 3 bits are permissions (read=4, write=2, execute=1).
    /// Bit 31 (`NAME_IS_USER`) indicates this is a user entry.
    pub access: u32,
}

impl IdAccess {
    /// Creates a new user ACL entry.
    #[must_use]
    pub const fn user(uid: u32, access: u32) -> Self {
        Self {
            id: uid,
            access: access | super::constants::NAME_IS_USER,
        }
    }

    /// Creates a new group ACL entry.
    #[must_use]
    pub const fn group(gid: u32, access: u32) -> Self {
        Self { id: gid, access }
    }

    /// Returns `true` if this is a user entry (vs group).
    #[must_use]
    pub const fn is_user(&self) -> bool {
        self.access & super::constants::NAME_IS_USER != 0
    }

    /// Returns the permission bits without the `NAME_IS_USER` flag.
    #[must_use]
    pub const fn permissions(&self) -> u32 {
        self.access & !super::constants::NAME_IS_USER
    }
}

/// List of named user/group ACL entries.
///
/// Corresponds to upstream's `ida_entries` struct in `acls.c` lines 62-65.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IdaEntries {
    entries: Vec<IdAccess>,
}

impl IdaEntries {
    /// Creates an empty entry list.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates an entry list with the given capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    /// Adds an entry to the list.
    pub fn push(&mut self, entry: IdAccess) {
        self.entries.push(entry);
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns an iterator over the entries.
    pub fn iter(&self) -> impl Iterator<Item = &IdAccess> {
        self.entries.iter()
    }

    /// Computes the combined mask bits from all entries.
    ///
    /// Used by the receiver to compute the effective mask when
    /// `XMIT_MASK_OBJ` was not explicitly sent.
    #[must_use]
    pub fn computed_mask_bits(&self) -> u8 {
        let mut mask: u8 = 0;
        for entry in &self.entries {
            mask |= entry.permissions() as u8;
        }
        mask & !NO_ENTRY
    }
}

impl FromIterator<IdAccess> for IdaEntries {
    fn from_iter<T: IntoIterator<Item = IdAccess>>(iter: T) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

/// Complete rsync ACL representation.
///
/// Contains both the standard POSIX ACL entries (user_obj, group_obj,
/// mask_obj, other_obj) and the list of named user/group entries.
///
/// # Upstream Reference
///
/// Corresponds to `rsync_acl` struct in `acls.c` lines 72-79.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RsyncAcl {
    /// Named user and group entries.
    pub names: IdaEntries,
    /// Owner (user object) permission bits, or `NO_ENTRY` if absent.
    pub user_obj: u8,
    /// Owning group (group object) permission bits, or `NO_ENTRY` if absent.
    pub group_obj: u8,
    /// ACL mask permission bits, or `NO_ENTRY` if absent.
    pub mask_obj: u8,
    /// Other (world) permission bits, or `NO_ENTRY` if absent.
    pub other_obj: u8,
}

impl Default for RsyncAcl {
    /// Creates an empty ACL with all entries set to `NO_ENTRY`.
    ///
    /// Matches upstream's `empty_rsync_acl` at `acls.c` lines 86-88.
    fn default() -> Self {
        Self {
            names: IdaEntries::new(),
            user_obj: NO_ENTRY,
            group_obj: NO_ENTRY,
            mask_obj: NO_ENTRY,
            other_obj: NO_ENTRY,
        }
    }
}

impl RsyncAcl {
    /// Creates a new empty ACL.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the user_obj entry is present.
    #[must_use]
    pub const fn has_user_obj(&self) -> bool {
        self.user_obj != NO_ENTRY
    }

    /// Returns `true` if the group_obj entry is present.
    #[must_use]
    pub const fn has_group_obj(&self) -> bool {
        self.group_obj != NO_ENTRY
    }

    /// Returns `true` if the mask_obj entry is present.
    #[must_use]
    pub const fn has_mask_obj(&self) -> bool {
        self.mask_obj != NO_ENTRY
    }

    /// Returns `true` if the other_obj entry is present.
    #[must_use]
    pub const fn has_other_obj(&self) -> bool {
        self.other_obj != NO_ENTRY
    }

    /// Returns `true` if this ACL has any content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.has_user_obj()
            && !self.has_group_obj()
            && !self.has_mask_obj()
            && !self.has_other_obj()
            && self.names.is_empty()
    }

    /// Computes the flags byte for wire encoding.
    ///
    /// Returns the `XMIT_*` flags indicating which entries are present.
    #[must_use]
    pub fn flags(&self) -> u8 {
        let mut flags = 0u8;
        if self.has_user_obj() {
            flags |= super::constants::XMIT_USER_OBJ;
        }
        if self.has_group_obj() {
            flags |= super::constants::XMIT_GROUP_OBJ;
        }
        if self.has_mask_obj() {
            flags |= super::constants::XMIT_MASK_OBJ;
        }
        if self.has_other_obj() {
            flags |= super::constants::XMIT_OTHER_OBJ;
        }
        if !self.names.is_empty() {
            flags |= super::constants::XMIT_NAME_LIST;
        }
        flags
    }
}

/// Cache for tracking sent/received ACLs.
///
/// Rsync uses index-based caching to avoid re-transmitting identical ACLs.
/// When an ACL matches a previously sent one, only its index is transmitted.
#[derive(Clone, Debug, Default)]
pub struct AclCache {
    access_acls: Vec<RsyncAcl>,
    default_acls: Vec<RsyncAcl>,
}

impl AclCache {
    /// Creates an empty ACL cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            access_acls: Vec::new(),
            default_acls: Vec::new(),
        }
    }

    /// Finds a matching access ACL in the cache.
    ///
    /// Returns the index if found, or `None` if no match.
    #[must_use]
    pub fn find_access(&self, acl: &RsyncAcl) -> Option<u32> {
        self.access_acls
            .iter()
            .position(|cached| cached == acl)
            .map(|idx| idx as u32)
    }

    /// Finds a matching default ACL in the cache.
    ///
    /// Returns the index if found, or `None` if no match.
    #[must_use]
    pub fn find_default(&self, acl: &RsyncAcl) -> Option<u32> {
        self.default_acls
            .iter()
            .position(|cached| cached == acl)
            .map(|idx| idx as u32)
    }

    /// Stores an access ACL in the cache.
    ///
    /// Returns the assigned index.
    pub fn store_access(&mut self, acl: RsyncAcl) -> u32 {
        let index = self.access_acls.len() as u32;
        self.access_acls.push(acl);
        index
    }

    /// Stores a default ACL in the cache.
    ///
    /// Returns the assigned index.
    pub fn store_default(&mut self, acl: RsyncAcl) -> u32 {
        let index = self.default_acls.len() as u32;
        self.default_acls.push(acl);
        index
    }

    /// Retrieves an access ACL by index.
    #[must_use]
    pub fn get_access(&self, index: u32) -> Option<&RsyncAcl> {
        self.access_acls.get(index as usize)
    }

    /// Retrieves a default ACL by index.
    #[must_use]
    pub fn get_default(&self, index: u32) -> Option<&RsyncAcl> {
        self.default_acls.get(index as usize)
    }

    /// Returns the number of cached access ACLs.
    #[must_use]
    pub fn access_count(&self) -> usize {
        self.access_acls.len()
    }

    /// Returns the number of cached default ACLs.
    #[must_use]
    pub fn default_count(&self) -> usize {
        self.default_acls.len()
    }
}
