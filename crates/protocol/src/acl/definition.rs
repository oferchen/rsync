//! High-level ACL definition types and wire parsing.
//!
//! Provides a POSIX-oriented view of ACL data as parsed from the rsync wire
//! protocol. These types sit above the raw wire-level structures (`RsyncAcl`,
//! `IdAccess`) and present a unified `AclEntry` list that is easier to work
//! with for permission application and display.
//!
//! # Wire Format
//!
//! When a new ACL is transmitted (not a cache reference), the sender writes:
//!
//! ```text
//! flags      : u8       // XMIT_* bits indicating which standard entries exist
//! [user_obj] : varint   // owner permissions (if XMIT_USER_OBJ)
//! [group_obj]: varint   // owning group permissions (if XMIT_GROUP_OBJ)
//! [mask_obj] : varint   // mask permissions (if XMIT_MASK_OBJ)
//! [other_obj]: varint   // world permissions (if XMIT_OTHER_OBJ)
//! [ida_list] : ...      // named user/group entries (if XMIT_NAME_LIST)
//! ```
//!
//! `read_acl_definition` parses this format and returns a flat `AclDefinition`
//! containing all entries as `AclEntry` values with unified `AclTag` tags.
//!
//! # Upstream Reference
//!
//! - `acls.c` lines 731-800: `recv_rsync_acl()`

use std::io::{self, Read};

use crate::varint::read_varint;

use super::constants::{
    XMIT_GROUP_OBJ, XMIT_MASK_OBJ, XMIT_NAME_LIST, XMIT_OTHER_OBJ, XMIT_USER_OBJ,
};
use super::entry::RsyncAcl;
use super::wire::recv_ida_entries;

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

/// A complete ACL definition as parsed from the wire.
///
/// Contains a list of ACL entries and a flag indicating whether an explicit
/// mask entry was present in the wire data. When named user/group entries
/// exist but no mask was explicitly sent, the receiver computes the mask
/// from the union of all named entry permissions.
///
/// # Upstream Reference
///
/// Corresponds to the literal data branch of `recv_rsync_acl()` in
/// `acls.c` lines 731-800.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AclDefinition {
    /// All entries in the ACL, in wire order.
    ///
    /// Standard entries (UserObj, GroupObj, Mask, Other) appear first,
    /// followed by named user/group entries.
    entries: Vec<AclEntry>,
    /// Whether an explicit mask entry was present on the wire.
    ///
    /// When false and named entries exist, the mask was computed from
    /// the union of all named entry permissions.
    mask_set: bool,
}

impl AclDefinition {
    /// Creates an empty ACL definition.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            mask_set: false,
        }
    }

    /// Returns true if an explicit mask entry was present on the wire.
    #[must_use]
    pub const fn mask_set(&self) -> bool {
        self.mask_set
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the definition has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a slice of all entries.
    #[must_use]
    pub fn entries(&self) -> &[AclEntry] {
        &self.entries
    }

    /// Consumes the definition and returns the entries as a vector.
    #[must_use]
    pub fn into_entries(self) -> Vec<AclEntry> {
        self.entries
    }

    /// Returns an iterator over the entries.
    pub fn iter(&self) -> impl Iterator<Item = &AclEntry> {
        self.entries.iter()
    }

    /// Converts a wire-level `RsyncAcl` into a high-level `AclDefinition`.
    ///
    /// Translates the separate standard entry fields and ida_entries list
    /// into a flat list of `AclEntry` values with unified `AclTag` tags.
    #[must_use]
    pub fn from_rsync_acl(acl: &RsyncAcl) -> Self {
        let mut entries = Vec::new();
        let mask_set = acl.has_mask_obj();

        if acl.has_user_obj() {
            entries.push(AclEntry::new(
                AclTag::UserObj,
                AclPerms::from_bits(acl.user_obj),
            ));
        }
        if acl.has_group_obj() {
            entries.push(AclEntry::new(
                AclTag::GroupObj,
                AclPerms::from_bits(acl.group_obj),
            ));
        }
        if acl.has_mask_obj() {
            entries.push(AclEntry::new(
                AclTag::Mask,
                AclPerms::from_bits(acl.mask_obj),
            ));
        }
        if acl.has_other_obj() {
            entries.push(AclEntry::new(
                AclTag::Other,
                AclPerms::from_bits(acl.other_obj),
            ));
        }

        for ida in acl.names.iter() {
            let tag = if ida.is_user() {
                AclTag::User(ida.id)
            } else {
                AclTag::Group(ida.id)
            };
            let perms = AclPerms::from_bits(ida.permissions() as u8);
            entries.push(AclEntry::new(tag, perms));
        }

        Self { entries, mask_set }
    }
}

impl<'a> IntoIterator for &'a AclDefinition {
    type Item = &'a AclEntry;
    type IntoIter = std::slice::Iter<'a, AclEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl IntoIterator for AclDefinition {
    type Item = AclEntry;
    type IntoIter = std::vec::IntoIter<AclEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// Reads an ACL definition from the wire.
///
/// Parses the literal ACL data that follows a cache-miss index (ndx < 0)
/// during file list transfer. Reads the flags byte, standard permission
/// entries, and named user/group entries, returning a unified
/// `AclDefinition` with all entries as `AclEntry` values.
///
/// This function reads the ACL body only - the caller must have already
/// read and dispatched on the cache index varint. Use `recv_rsync_acl`
/// for the full index-or-literal dispatch.
///
/// # Wire Format
///
/// ```text
/// flags      : u8       // XMIT_* bits for standard entries
/// [user_obj] : varint   // if XMIT_USER_OBJ
/// [group_obj]: varint   // if XMIT_GROUP_OBJ
/// [mask_obj] : varint   // if XMIT_MASK_OBJ
/// [other_obj]: varint   // if XMIT_OTHER_OBJ
/// [ida_list] :          // if XMIT_NAME_LIST
///   count    : varint
///   entries  : count x (id: varint, access: varint, [name])
/// ```
///
/// # Upstream Reference
///
/// Mirrors the literal-data branch of `recv_rsync_acl()` in `acls.c`
/// lines 740-800.
pub fn read_acl_definition<R: Read>(reader: &mut R) -> io::Result<AclDefinition> {
    let mut flags_buf = [0u8; 1];
    reader.read_exact(&mut flags_buf)?;
    let flags = flags_buf[0];

    let mut entries = Vec::new();
    let mut explicit_mask = false;

    if flags & XMIT_USER_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::UserObj, AclPerms::from_bits(perms)));
    }
    if flags & XMIT_GROUP_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::GroupObj, AclPerms::from_bits(perms)));
    }
    if flags & XMIT_MASK_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::Mask, AclPerms::from_bits(perms)));
        explicit_mask = true;
    }
    if flags & XMIT_OTHER_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::Other, AclPerms::from_bits(perms)));
    }

    if flags & XMIT_NAME_LIST != 0 {
        let (ida_entries, computed_mask) = recv_ida_entries(reader)?;

        for ida in ida_entries.iter() {
            let tag = if ida.is_user() {
                AclTag::User(ida.id)
            } else {
                AclTag::Group(ida.id)
            };
            let perms = AclPerms::from_bits(ida.permissions() as u8);
            entries.push(AclEntry::new(tag, perms));
        }

        // upstream: acls.c recv_rsync_acl() sets mask from computed value
        // when named entries exist but no explicit mask was transmitted
        if !ida_entries.is_empty() && !explicit_mask {
            entries.push(AclEntry::new(
                AclTag::Mask,
                AclPerms::from_bits(computed_mask),
            ));
        }
    }

    Ok(AclDefinition {
        entries,
        mask_set: explicit_mask,
    })
}

/// Writes an ACL definition to the wire.
///
/// Encodes the ACL entries as the flags byte followed by standard entries
/// and named user/group entries. This writes the ACL body only - the
/// caller is responsible for writing the cache index varint first.
///
/// # Upstream Reference
///
/// Mirrors the literal-data branch of `send_rsync_acl()` in `acls.c`
/// lines 610-647.
pub fn write_acl_definition<W: io::Write>(
    writer: &mut W,
    definition: &AclDefinition,
) -> io::Result<()> {
    use super::entry::{IdAccess, IdaEntries};
    use super::wire::send_ida_entries;
    use crate::varint::write_varint;

    // Build the wire-level RsyncAcl from the definition
    let mut user_obj: Option<u8> = None;
    let mut group_obj: Option<u8> = None;
    let mut mask_obj: Option<u8> = None;
    let mut other_obj: Option<u8> = None;
    let mut named = IdaEntries::new();

    for entry in &definition.entries {
        match entry.tag {
            AclTag::UserObj => user_obj = Some(entry.perms.bits()),
            AclTag::GroupObj => group_obj = Some(entry.perms.bits()),
            AclTag::Mask => mask_obj = Some(entry.perms.bits()),
            AclTag::Other => other_obj = Some(entry.perms.bits()),
            AclTag::User(uid) => {
                named.push(IdAccess::user(uid, u32::from(entry.perms.bits())));
            }
            AclTag::Group(gid) => {
                named.push(IdAccess::group(gid, u32::from(entry.perms.bits())));
            }
        }
    }

    // Compute flags byte
    let mut flags = 0u8;
    if user_obj.is_some() {
        flags |= XMIT_USER_OBJ;
    }
    if group_obj.is_some() {
        flags |= XMIT_GROUP_OBJ;
    }
    if mask_obj.is_some() {
        flags |= XMIT_MASK_OBJ;
    }
    if other_obj.is_some() {
        flags |= XMIT_OTHER_OBJ;
    }
    if !named.is_empty() {
        flags |= XMIT_NAME_LIST;
    }

    writer.write_all(&[flags])?;

    if let Some(perms) = user_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if let Some(perms) = group_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if let Some(perms) = mask_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if let Some(perms) = other_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if !named.is_empty() {
        send_ida_entries(writer, &named, false)?;
    }

    Ok(())
}

#[cfg(test)]
mod definition_tests {
    use super::*;
    use crate::varint::write_varint;
    use std::io::Cursor;

    use super::super::constants::ACCESS_SHIFT;
    use super::super::entry::{IdAccess, RsyncAcl};

    // --- AclPerms tests ---

    #[test]
    fn perms_from_bits_masks_to_three_bits() {
        assert_eq!(AclPerms::from_bits(0xFF).bits(), 0x07);
        assert_eq!(AclPerms::from_bits(0x00).bits(), 0x00);
        assert_eq!(AclPerms::from_bits(0x05).bits(), 0x05);
    }

    #[test]
    fn perms_read_write_execute() {
        let rwx = AclPerms::from_bits(7);
        assert!(rwx.read());
        assert!(rwx.write());
        assert!(rwx.execute());

        let r_only = AclPerms::from_bits(4);
        assert!(r_only.read());
        assert!(!r_only.write());
        assert!(!r_only.execute());

        let none = AclPerms::from_bits(0);
        assert!(!none.read());
        assert!(!none.write());
        assert!(!none.execute());
    }

    #[test]
    fn perms_display() {
        assert_eq!(format!("{}", AclPerms::from_bits(7)), "rwx");
        assert_eq!(format!("{}", AclPerms::from_bits(5)), "r-x");
        assert_eq!(format!("{}", AclPerms::from_bits(6)), "rw-");
        assert_eq!(format!("{}", AclPerms::from_bits(0)), "---");
        assert_eq!(format!("{}", AclPerms::from_bits(1)), "--x");
    }

    // --- AclTag tests ---

    #[test]
    fn tag_equality() {
        assert_eq!(AclTag::UserObj, AclTag::UserObj);
        assert_ne!(AclTag::UserObj, AclTag::GroupObj);
        assert_eq!(AclTag::User(1000), AclTag::User(1000));
        assert_ne!(AclTag::User(1000), AclTag::User(1001));
        assert_ne!(AclTag::User(1000), AclTag::Group(1000));
    }

    #[test]
    fn tag_debug_format() {
        assert!(format!("{:?}", AclTag::UserObj).contains("UserObj"));
        assert!(format!("{:?}", AclTag::User(42)).contains("42"));
        assert!(format!("{:?}", AclTag::Group(100)).contains("Group"));
    }

    // --- AclEntry tests ---

    #[test]
    fn entry_construction() {
        let entry = AclEntry::new(AclTag::UserObj, AclPerms::from_bits(7));
        assert_eq!(entry.tag, AclTag::UserObj);
        assert_eq!(entry.perms.bits(), 7);
    }

    // --- AclDefinition tests ---

    #[test]
    fn empty_definition() {
        let def = AclDefinition::new();
        assert!(def.is_empty());
        assert_eq!(def.len(), 0);
        assert!(!def.mask_set());
        assert!(def.entries().is_empty());
    }

    #[test]
    fn from_rsync_acl_minimal() {
        let acl = RsyncAcl::from_mode(0o755);
        let def = AclDefinition::from_rsync_acl(&acl);

        assert_eq!(def.len(), 3); // user_obj, group_obj, other_obj
        assert!(!def.mask_set());
        assert_eq!(def.entries()[0].tag, AclTag::UserObj);
        assert_eq!(def.entries()[0].perms.bits(), 7); // rwx
        assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
        assert_eq!(def.entries()[1].perms.bits(), 5); // r-x
        assert_eq!(def.entries()[2].tag, AclTag::Other);
        assert_eq!(def.entries()[2].perms.bits(), 5); // r-x
    }

    #[test]
    fn from_rsync_acl_with_mask_and_named_entries() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 7;
        acl.group_obj = 5;
        acl.mask_obj = 7;
        acl.other_obj = 0;
        acl.names.push(IdAccess::user(1000, 7));
        acl.names.push(IdAccess::group(100, 5));

        let def = AclDefinition::from_rsync_acl(&acl);
        assert_eq!(def.len(), 6);
        assert!(def.mask_set());

        // Standard entries
        assert_eq!(def.entries()[0].tag, AclTag::UserObj);
        assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
        assert_eq!(def.entries()[2].tag, AclTag::Mask);
        assert_eq!(def.entries()[3].tag, AclTag::Other);

        // Named entries
        assert_eq!(def.entries()[4].tag, AclTag::User(1000));
        assert_eq!(def.entries()[4].perms.bits(), 7);
        assert_eq!(def.entries()[5].tag, AclTag::Group(100));
        assert_eq!(def.entries()[5].perms.bits(), 5);
    }

    #[test]
    fn from_rsync_acl_empty() {
        let acl = RsyncAcl::new();
        let def = AclDefinition::from_rsync_acl(&acl);
        assert!(def.is_empty());
        assert!(!def.mask_set());
    }

    #[test]
    fn definition_into_entries() {
        let acl = RsyncAcl::from_mode(0o644);
        let def = AclDefinition::from_rsync_acl(&acl);
        let entries = def.into_entries();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn definition_iter() {
        let acl = RsyncAcl::from_mode(0o700);
        let def = AclDefinition::from_rsync_acl(&acl);
        let tags: Vec<_> = def.iter().map(|e| e.tag).collect();
        assert_eq!(tags, vec![AclTag::UserObj, AclTag::GroupObj, AclTag::Other]);
    }

    #[test]
    fn definition_into_iterator_ref() {
        let acl = RsyncAcl::from_mode(0o750);
        let def = AclDefinition::from_rsync_acl(&acl);
        let count = (&def).into_iter().count();
        assert_eq!(count, 3);
    }

    #[test]
    fn definition_into_iterator_owned() {
        let acl = RsyncAcl::from_mode(0o750);
        let def = AclDefinition::from_rsync_acl(&acl);
        let entries: Vec<_> = def.into_iter().collect();
        assert_eq!(entries.len(), 3);
    }

    // --- Wire parsing tests ---

    /// Helper: builds wire bytes for a flags-only ACL (no entries).
    fn wire_empty_acl() -> Vec<u8> {
        vec![0x00] // flags = 0, no entries
    }

    /// Helper: builds wire bytes for an ACL with standard entries only.
    fn wire_standard_acl(user: u8, group: u8, other: u8) -> Vec<u8> {
        let flags = XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_OTHER_OBJ;
        let mut data = vec![flags];
        // Each standard entry is a varint; single-byte for values 0-7
        data.push(user);
        data.push(group);
        data.push(other);
        data
    }

    /// Helper: builds wire bytes for an ACL with mask.
    fn wire_acl_with_mask(user: u8, group: u8, mask: u8, other: u8) -> Vec<u8> {
        let flags = XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_MASK_OBJ | XMIT_OTHER_OBJ;
        let mut data = vec![flags];
        data.push(user);
        data.push(group);
        data.push(mask);
        data.push(other);
        data
    }

    #[test]
    fn read_empty_acl() {
        let data = wire_empty_acl();
        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert!(def.is_empty());
        assert!(!def.mask_set());
    }

    #[test]
    fn read_standard_entries_only() {
        let data = wire_standard_acl(7, 5, 5);
        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert_eq!(def.len(), 3);
        assert!(!def.mask_set());

        assert_eq!(def.entries()[0].tag, AclTag::UserObj);
        assert_eq!(def.entries()[0].perms.bits(), 7);
        assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
        assert_eq!(def.entries()[1].perms.bits(), 5);
        assert_eq!(def.entries()[2].tag, AclTag::Other);
        assert_eq!(def.entries()[2].perms.bits(), 5);
    }

    #[test]
    fn read_acl_with_explicit_mask() {
        let data = wire_acl_with_mask(7, 7, 5, 4);
        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert_eq!(def.len(), 4);
        assert!(def.mask_set());

        assert_eq!(def.entries()[0].tag, AclTag::UserObj);
        assert_eq!(def.entries()[0].perms.bits(), 7);
        assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
        assert_eq!(def.entries()[1].perms.bits(), 7);
        assert_eq!(def.entries()[2].tag, AclTag::Mask);
        assert_eq!(def.entries()[2].perms.bits(), 5);
        assert_eq!(def.entries()[3].tag, AclTag::Other);
        assert_eq!(def.entries()[3].perms.bits(), 4);
    }

    #[test]
    fn read_acl_with_named_entries() {
        let mut data = Vec::new();

        // flags: user_obj + group_obj + other_obj + name_list
        let flags = XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_OTHER_OBJ | XMIT_NAME_LIST;
        data.push(flags);

        // Standard entries (single-byte varints)
        data.push(7); // user_obj = rwx
        data.push(5); // group_obj = r-x
        data.push(4); // other_obj = r--

        // ida_entries: count=2
        write_varint(&mut data, 2).unwrap();

        // Entry 1: user uid=1000, perms=rwx
        write_varint(&mut data, 1000).unwrap();
        let encoded = (0x07u32 << ACCESS_SHIFT) | super::super::constants::XFLAG_NAME_IS_USER;
        write_varint(&mut data, encoded as i32).unwrap();

        // Entry 2: group gid=100, perms=r-x
        write_varint(&mut data, 100).unwrap();
        let encoded = 0x05u32 << ACCESS_SHIFT;
        write_varint(&mut data, encoded as i32).unwrap();

        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        // 3 standard + 2 named + 1 computed mask = 6
        assert_eq!(def.len(), 6);
        assert!(!def.mask_set()); // mask was computed, not explicit

        assert_eq!(def.entries()[0].tag, AclTag::UserObj);
        assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
        assert_eq!(def.entries()[2].tag, AclTag::Other);
        assert_eq!(def.entries()[3].tag, AclTag::User(1000));
        assert_eq!(def.entries()[3].perms.bits(), 7);
        assert_eq!(def.entries()[4].tag, AclTag::Group(100));
        assert_eq!(def.entries()[4].perms.bits(), 5);

        // Computed mask should be union of named entry permissions: 7 | 5 = 7
        assert_eq!(def.entries()[5].tag, AclTag::Mask);
        assert_eq!(def.entries()[5].perms.bits(), 7);
    }

    #[test]
    fn read_acl_with_named_entries_and_explicit_mask() {
        let mut data = Vec::new();

        let flags =
            XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_MASK_OBJ | XMIT_OTHER_OBJ | XMIT_NAME_LIST;
        data.push(flags);

        data.push(7); // user_obj
        data.push(7); // group_obj
        data.push(5); // mask_obj (explicit)
        data.push(0); // other_obj

        // ida_entries: count=1
        write_varint(&mut data, 1).unwrap();

        // Entry: user uid=500, perms=rwx
        write_varint(&mut data, 500).unwrap();
        let encoded = (0x07u32 << ACCESS_SHIFT) | super::super::constants::XFLAG_NAME_IS_USER;
        write_varint(&mut data, encoded as i32).unwrap();

        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        // 4 standard + 1 named = 5 (no computed mask because explicit mask exists)
        assert_eq!(def.len(), 5);
        assert!(def.mask_set());

        assert_eq!(def.entries()[2].tag, AclTag::Mask);
        assert_eq!(def.entries()[2].perms.bits(), 5);
        assert_eq!(def.entries()[4].tag, AclTag::User(500));
    }

    #[test]
    fn read_acl_user_obj_only() {
        let data = vec![XMIT_USER_OBJ, 6]; // rw-
        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert_eq!(def.len(), 1);
        assert_eq!(def.entries()[0].tag, AclTag::UserObj);
        assert_eq!(def.entries()[0].perms.bits(), 6);
    }

    #[test]
    fn read_acl_mask_only() {
        let data = vec![XMIT_MASK_OBJ, 5]; // r-x
        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert_eq!(def.len(), 1);
        assert!(def.mask_set());
        assert_eq!(def.entries()[0].tag, AclTag::Mask);
        assert_eq!(def.entries()[0].perms.bits(), 5);
    }

    #[test]
    fn read_acl_other_obj_only() {
        let data = vec![XMIT_OTHER_OBJ, 4]; // r--
        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert_eq!(def.len(), 1);
        assert_eq!(def.entries()[0].tag, AclTag::Other);
        assert_eq!(def.entries()[0].perms.bits(), 4);
    }

    #[test]
    fn read_acl_all_permission_combinations() {
        for perms in 0..=7u8 {
            let data = vec![XMIT_USER_OBJ, perms];
            let mut cursor = Cursor::new(data);
            let def = read_acl_definition(&mut cursor).unwrap();

            assert_eq!(def.entries()[0].perms.bits(), perms);
        }
    }

    // --- EOF error tests ---

    #[test]
    fn read_eof_on_flags() {
        let data: Vec<u8> = vec![];
        let mut cursor = Cursor::new(data);
        assert!(read_acl_definition(&mut cursor).is_err());
    }

    #[test]
    fn read_eof_on_user_obj() {
        let data = vec![XMIT_USER_OBJ]; // flags say user_obj but no data
        let mut cursor = Cursor::new(data);
        assert!(read_acl_definition(&mut cursor).is_err());
    }

    #[test]
    fn read_eof_on_group_obj() {
        let data = vec![XMIT_USER_OBJ | XMIT_GROUP_OBJ, 7]; // user_obj ok, group_obj missing
        let mut cursor = Cursor::new(data);
        assert!(read_acl_definition(&mut cursor).is_err());
    }

    #[test]
    fn read_eof_on_mask_obj() {
        let data = vec![XMIT_MASK_OBJ]; // flags say mask but no data
        let mut cursor = Cursor::new(data);
        assert!(read_acl_definition(&mut cursor).is_err());
    }

    #[test]
    fn read_eof_on_other_obj() {
        let data = vec![XMIT_OTHER_OBJ]; // flags say other but no data
        let mut cursor = Cursor::new(data);
        assert!(read_acl_definition(&mut cursor).is_err());
    }

    #[test]
    fn read_eof_on_ida_count() {
        let data = vec![XMIT_NAME_LIST]; // flags say name list but no count
        let mut cursor = Cursor::new(data);
        assert!(read_acl_definition(&mut cursor).is_err());
    }

    // --- Roundtrip tests ---

    #[test]
    fn roundtrip_empty_acl() {
        let original = AclDefinition::new();
        let mut buf = Vec::new();
        write_acl_definition(&mut buf, &original).unwrap();

        let mut cursor = Cursor::new(buf);
        let parsed = read_acl_definition(&mut cursor).unwrap();

        assert!(parsed.is_empty());
    }

    #[test]
    fn roundtrip_standard_entries() {
        let acl = RsyncAcl::from_mode(0o755);
        let original = AclDefinition::from_rsync_acl(&acl);

        let mut buf = Vec::new();
        write_acl_definition(&mut buf, &original).unwrap();

        let mut cursor = Cursor::new(buf);
        let parsed = read_acl_definition(&mut cursor).unwrap();

        assert_eq!(parsed.len(), original.len());
        for (a, b) in parsed.entries().iter().zip(original.entries().iter()) {
            assert_eq!(a.tag, b.tag);
            assert_eq!(a.perms.bits(), b.perms.bits());
        }
    }

    #[test]
    fn roundtrip_with_mask() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 7;
        acl.group_obj = 7;
        acl.mask_obj = 5;
        acl.other_obj = 4;

        let original = AclDefinition::from_rsync_acl(&acl);
        assert!(original.mask_set());

        let mut buf = Vec::new();
        write_acl_definition(&mut buf, &original).unwrap();

        let mut cursor = Cursor::new(buf);
        let parsed = read_acl_definition(&mut cursor).unwrap();

        assert!(parsed.mask_set());
        assert_eq!(parsed.len(), original.len());
        for (a, b) in parsed.entries().iter().zip(original.entries().iter()) {
            assert_eq!(a.tag, b.tag);
            assert_eq!(a.perms.bits(), b.perms.bits());
        }
    }

    #[test]
    fn roundtrip_with_named_entries() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 7;
        acl.group_obj = 5;
        acl.mask_obj = 7;
        acl.other_obj = 0;
        acl.names.push(IdAccess::user(1000, 7));
        acl.names.push(IdAccess::group(100, 5));

        let original = AclDefinition::from_rsync_acl(&acl);

        let mut buf = Vec::new();
        write_acl_definition(&mut buf, &original).unwrap();

        let mut cursor = Cursor::new(buf);
        let parsed = read_acl_definition(&mut cursor).unwrap();

        // The roundtrip may differ slightly in mask handling because
        // write uses the entries as-is but read may add computed mask.
        // With explicit mask in original, the roundtrip should be exact.
        assert!(parsed.mask_set());
        assert_eq!(parsed.entries()[0].tag, AclTag::UserObj);
        assert_eq!(parsed.entries()[1].tag, AclTag::GroupObj);
        assert_eq!(parsed.entries()[2].tag, AclTag::Mask);
        assert_eq!(parsed.entries()[3].tag, AclTag::Other);
        assert_eq!(parsed.entries()[4].tag, AclTag::User(1000));
        assert_eq!(parsed.entries()[5].tag, AclTag::Group(100));
    }

    #[test]
    fn named_entries_empty_list_no_computed_mask() {
        // XMIT_NAME_LIST set but count=0 - no computed mask added
        let mut data = Vec::new();
        data.push(XMIT_NAME_LIST);
        write_varint(&mut data, 0).unwrap(); // count = 0

        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert!(def.is_empty());
        assert!(!def.mask_set());
    }

    #[test]
    fn perms_default_is_zero() {
        let p = AclPerms::default();
        assert_eq!(p.bits(), 0);
        assert!(!p.read());
        assert!(!p.write());
        assert!(!p.execute());
    }

    #[test]
    fn definition_default_is_empty() {
        let def = AclDefinition::default();
        assert!(def.is_empty());
        assert!(!def.mask_set());
    }
}
