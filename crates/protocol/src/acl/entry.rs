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
    /// Optional user or group name for wire transmission.
    ///
    /// When `include_names` is set in `send_ida_entries`, entries with a name
    /// will have the `XFLAG_NAME_FOLLOWS` flag set and the name bytes written
    /// after the access varint.
    ///
    /// # Upstream Reference
    ///
    /// In upstream rsync, names are resolved from UID/GID via `uid_to_name()`
    /// and `gid_to_name()` before sending. The receiver uses names for
    /// UID/GID remapping on the destination system.
    pub name: Option<Vec<u8>>,
}

impl IdAccess {
    /// Creates a new user ACL entry.
    #[must_use]
    pub const fn user(uid: u32, access: u32) -> Self {
        Self {
            id: uid,
            access: access | super::constants::NAME_IS_USER,
            name: None,
        }
    }

    /// Creates a new group ACL entry.
    #[must_use]
    pub const fn group(gid: u32, access: u32) -> Self {
        Self {
            id: gid,
            access,
            name: None,
        }
    }

    /// Creates a new user ACL entry with a resolved name.
    #[must_use]
    pub fn user_with_name(uid: u32, access: u32, name: Vec<u8>) -> Self {
        Self {
            id: uid,
            access: access | super::constants::NAME_IS_USER,
            name: Some(name),
        }
    }

    /// Creates a new group ACL entry with a resolved name.
    #[must_use]
    pub fn group_with_name(gid: u32, access: u32, name: Vec<u8>) -> Self {
        Self {
            id: gid,
            access,
            name: Some(name),
        }
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

    /// Encodes this ACL into upstream's `--fake-super` xattr byte format.
    ///
    /// Four little-endian `u32` base fields (`user_obj`, `group_obj`,
    /// `mask_obj`, `other_obj` - zero-extended from the `u8` permission value
    /// or the `NO_ENTRY` sentinel), followed by one 8-byte `(id: u32 LE,
    /// access: u32 LE)` record per named entry in `names`. `access` is the
    /// raw in-memory value, including the `NAME_IS_USER` high bit.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors the blob written by `set_rsync_acl()` (`acls.c` lines 933-970)
    /// via `set_xattr_acl()` (`xattrs.c` lines 1112-1125).
    #[must_use]
    pub fn to_fake_super_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 + self.names.len() * 8);
        buf.extend_from_slice(&u32::from(self.user_obj).to_le_bytes());
        buf.extend_from_slice(&u32::from(self.group_obj).to_le_bytes());
        buf.extend_from_slice(&u32::from(self.mask_obj).to_le_bytes());
        buf.extend_from_slice(&u32::from(self.other_obj).to_le_bytes());
        for ida in self.names.iter() {
            buf.extend_from_slice(&ida.id.to_le_bytes());
            buf.extend_from_slice(&ida.access.to_le_bytes());
        }
        buf
    }

    /// Decodes an ACL from upstream's `--fake-super` xattr byte format.
    ///
    /// Returns `None` when `buf` is shorter than the 16-byte base header or
    /// its length is not `16 + 8*n` for some named-entry count `n`, mirroring
    /// `get_rsync_acl()`'s length validation (`acls.c` lines 483-486).
    ///
    /// # Upstream Reference
    ///
    /// Mirrors the `am_root < 0` branch of `get_rsync_acl()` in `acls.c`
    /// lines 472-509.
    #[must_use]
    pub fn from_fake_super_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 16 {
            return None;
        }
        let names_len = buf.len() - 16;
        if names_len % 8 != 0 {
            return None;
        }
        let count = names_len / 8;

        let u32_at = |offset: usize| -> u32 {
            u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap_or([0; 4]))
        };

        let mut acl = Self {
            names: IdaEntries::with_capacity(count),
            user_obj: u32_at(0) as u8,
            group_obj: u32_at(4) as u8,
            mask_obj: u32_at(8) as u8,
            other_obj: u32_at(12) as u8,
        };

        for i in 0..count {
            let offset = 16 + i * 8;
            acl.names.push(IdAccess {
                id: u32_at(offset),
                access: u32_at(offset + 4),
                name: None,
            });
        }

        Some(acl)
    }
}

/// POSIX ACL tag type for permission extraction from file mode bits.
///
/// Each tag type corresponds to a different position in the Unix file
/// mode word from which permission bits are extracted.
///
/// # Upstream Reference
///
/// Used by `rsync_acl_get_perms()` in `acls.c` lines 139-155.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AclTagType {
    /// Owner permissions - bits 8-6 of mode.
    UserObj,
    /// Owning group permissions - bits 5-3 of mode.
    GroupObj,
    /// ACL mask - same position as group (bits 5-3) per POSIX.1e.
    MaskObj,
    /// Other/world permissions - bits 2-0 of mode.
    OtherObj,
}

/// Extracts permission bits from a file mode for a given ACL tag type.
///
/// Maps Unix file mode bits to the 3-bit rwx permission value used in
/// ACL entries. The mask position overlaps with group per POSIX.1e
/// semantics - when an ACL has named entries, the group bits in the
/// file mode represent the mask, not the owning group permissions.
///
/// # Upstream Reference
///
/// Mirrors `rsync_acl_get_perms()` in `acls.c` lines 139-155.
#[must_use]
pub const fn get_perms(mode: u32, tag_type: AclTagType) -> u8 {
    let shift = match tag_type {
        AclTagType::UserObj => 6,
        // upstream: acls.c - mask uses same bits as group per POSIX.1e
        AclTagType::GroupObj | AclTagType::MaskObj => 3,
        AclTagType::OtherObj => 0,
    };
    ((mode >> shift) & 7) as u8
}

impl IdaEntries {
    /// Removes all entries from the list.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl RsyncAcl {
    /// Creates a minimal ACL from file mode bits.
    ///
    /// Populates user_obj, group_obj, and other_obj from the corresponding
    /// permission bits in the file mode. The mask_obj is left as `NO_ENTRY`
    /// and no named entries are added, producing the simplest ACL that
    /// represents the given mode.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `rsync_acl_fake_perms()` in `acls.c` lines 157-170.
    pub fn fake_perms(&mut self, mode: u32) {
        self.user_obj = get_perms(mode, AclTagType::UserObj);
        self.group_obj = get_perms(mode, AclTagType::GroupObj);
        self.other_obj = get_perms(mode, AclTagType::OtherObj);
        self.mask_obj = NO_ENTRY;
        self.names = IdaEntries::new();
    }

    /// Creates an ACL from file mode bits.
    ///
    /// Convenience constructor that builds a minimal ACL with user_obj,
    /// group_obj, and other_obj populated from the mode. Equivalent to
    /// creating a default ACL and calling `fake_perms`.
    ///
    /// # Upstream Reference
    ///
    /// Equivalent to `rsync_acl_fake_perms()` in `acls.c` lines 157-170,
    /// but as a standalone constructor.
    #[must_use]
    pub fn from_mode(mode: u32) -> Self {
        Self {
            names: IdaEntries::new(),
            user_obj: get_perms(mode, AclTagType::UserObj),
            group_obj: get_perms(mode, AclTagType::GroupObj),
            mask_obj: NO_ENTRY,
            other_obj: get_perms(mode, AclTagType::OtherObj),
        }
    }

    /// Strips an ACL down to just the base permission entries.
    ///
    /// Removes all named user/group entries and clears the mask_obj.
    /// After stripping, only user_obj, group_obj, and other_obj remain.
    /// If the ACL had a mask_obj, the group_obj is replaced with the
    /// mask value (since POSIX.1e stores effective group perms in mask
    /// when extended entries exist).
    pub fn strip_perms(&mut self) {
        if self.has_mask_obj() {
            self.group_obj = self.mask_obj;
            self.mask_obj = NO_ENTRY;
        }
        self.names.clear();
    }

    /// Removes permission entries that can be inferred from the file mode.
    ///
    /// Called before sending ACLs on the wire. The receiver reconstructs
    /// stripped entries from the file mode transmitted in the file list.
    /// This reduces wire traffic by omitting redundant data.
    ///
    /// The stripping rules:
    /// - `user_obj` is always stripped (derivable from mode bits 8-6)
    /// - `other_obj` is always stripped (derivable from mode bits 2-0)
    /// - When no mask is present, `group_obj` is stripped (derivable from bits 5-3)
    /// - When mask is present and `group_obj` matches the group bits from mode,
    ///   `group_obj` is stripped
    /// - When mask is present, named entries exist, and mask matches the group
    ///   bits from mode, `mask_obj` is stripped
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `rsync_acl_strip_perms()` in `acls.c` lines 138-155.
    pub fn strip_perms_for_send(&mut self, mode: u32) {
        // upstream: acls.c:142 - user_obj always stripped
        self.user_obj = NO_ENTRY;

        if self.mask_obj == NO_ENTRY {
            // upstream: acls.c:143-144 - no mask means group_obj is redundant
            self.group_obj = NO_ENTRY;
        } else {
            let group_perms = ((mode >> 3) & 7) as u8;
            // upstream: acls.c:147-148
            if self.group_obj == group_perms {
                self.group_obj = NO_ENTRY;
            }
            // upstream: acls.c:150-151 - mask stripped when it matches group perms
            // and named entries exist
            if !self.names.is_empty() && self.mask_obj == group_perms {
                self.mask_obj = NO_ENTRY;
            }
        }

        // upstream: acls.c:154 - other_obj always stripped
        self.other_obj = NO_ENTRY;
    }

    /// Compares two ACLs for semantic equivalence.
    ///
    /// Two ACLs are "equal enough" when they produce the same effective
    /// permissions. When neither ACL has named entries, the mask is
    /// irrelevant (it only limits named entry permissions), so mask
    /// differences are ignored in that case. Named entries (ida_entries)
    /// are compared element-by-element when present.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `rsync_acl_equal_enough()` in `acls.c` lines 282-332.
    #[must_use]
    pub fn equal_enough(&self, other: &RsyncAcl) -> bool {
        // upstream: acls.c:284-285 - compare user_obj and other_obj first
        if self.user_obj != other.user_obj {
            return false;
        }
        if self.other_obj != other.other_obj {
            return false;
        }

        // upstream: acls.c:292-295 - compare named entries
        if self.names.len() != other.names.len() {
            return false;
        }

        for (a, b) in self.names.iter().zip(other.names.iter()) {
            if a.id != b.id || a.access != b.access {
                return false;
            }
        }

        // upstream: acls.c:309-331 - mask and group_obj comparison depends
        // on whether named entries exist
        if self.names.is_empty() {
            // upstream: acls.c:310-315 - without named entries, mask is
            // irrelevant; the effective group is mask_obj if present, else
            // group_obj
            let self_group = if self.has_mask_obj() {
                self.mask_obj
            } else {
                self.group_obj
            };
            let other_group = if other.has_mask_obj() {
                other.mask_obj
            } else {
                other.group_obj
            };
            self_group == other_group
        } else {
            // upstream: acls.c:325-331 - with named entries, both mask and
            // group must match exactly
            self.group_obj == other.group_obj && self.mask_obj == other.mask_obj
        }
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
    /// Index where the previous access-ACL lookup matched, if any.
    ///
    /// Mirrors upstream's per-type `static` match cursor so a lookup resumes
    /// from the last hit (or the list end), since equal ACLs arrive in runs.
    last_access_match: Option<usize>,
    /// Index where the previous default-ACL lookup matched, if any.
    last_default_match: Option<usize>,
}

impl AclCache {
    /// Creates an empty ACL cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            access_acls: Vec::new(),
            default_acls: Vec::new(),
            last_access_match: None,
            last_default_match: None,
        }
    }

    /// Finds a matching access ACL in the cache.
    ///
    /// Returns the index if found, or `None` if no match. Because the store
    /// path only appends ACLs absent from the cache, at most one entry can
    /// match, so the returned index (and thus the wire output) is identical
    /// regardless of scan start.
    pub fn find_access(&mut self, acl: &RsyncAcl) -> Option<u32> {
        Self::find_matching(&self.access_acls, acl, &mut self.last_access_match)
    }

    /// Finds a matching default ACL in the cache.
    ///
    /// Returns the index if found, or `None` if no match. See
    /// [`find_access`](Self::find_access) for why the resume cursor cannot
    /// change which index is returned.
    pub fn find_default(&mut self, acl: &RsyncAcl) -> Option<u32> {
        Self::find_matching(&self.default_acls, acl, &mut self.last_default_match)
    }

    /// Scans `acls` for one equal to `acl`, resuming from `last_match`.
    ///
    /// Starts at the previous match (or the list end on the first lookup or
    /// after a miss) and walks backward with wrap-around, since identical ACLs
    /// tend to arrive in consecutive runs. Updates `last_match` to the hit, or
    /// clears it on a miss.
    ///
    /// # Upstream Reference
    ///
    /// See `acls.c:find_matching_rsync_acl()` lines 448-470 - the same
    /// resume-at-last-match cursor over `rsync_acl_list`.
    fn find_matching(
        acls: &[RsyncAcl],
        acl: &RsyncAcl,
        last_match: &mut Option<usize>,
    ) -> Option<u32> {
        let count = acls.len();
        if count == 0 {
            *last_match = None;
            return None;
        }

        // upstream: start at the end of the list on the first pass or a prior miss.
        let mut idx = (*last_match).filter(|&i| i < count).unwrap_or(count - 1);
        for _ in 0..count {
            if acls[idx] == *acl {
                *last_match = Some(idx);
                return Some(idx as u32);
            }
            idx = if idx == 0 { count - 1 } else { idx - 1 };
        }

        *last_match = None;
        None
    }

    /// Stores an access ACL in the cache.
    ///
    /// Returns the assigned index.
    #[must_use]
    pub fn store_access(&mut self, acl: RsyncAcl) -> u32 {
        let index = self.access_acls.len() as u32;
        self.access_acls.push(acl);
        index
    }

    /// Stores a default ACL in the cache.
    ///
    /// Returns the assigned index.
    #[must_use]
    pub fn store_default(&mut self, acl: RsyncAcl) -> u32 {
        let index = self.default_acls.len() as u32;
        self.default_acls.push(acl);
        index
    }

    /// Retrieves an access ACL by index.
    pub fn get_access(&self, index: u32) -> Option<&RsyncAcl> {
        self.access_acls.get(index as usize)
    }

    /// Retrieves a default ACL by index.
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

    /// Iterates over every cached ACL (access then default).
    ///
    /// Used by the sender to feed each named entry's user/group id into the
    /// shared uid/gid id-list, mirroring upstream `add_uid`/`add_gid` in
    /// `send_ida_entries` (`acls.c:592-595`).
    pub fn iter_acls(&self) -> impl Iterator<Item = &RsyncAcl> {
        self.access_acls.iter().chain(self.default_acls.iter())
    }
}
