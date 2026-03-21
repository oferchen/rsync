//! ACL definition struct with conversion from wire-level `RsyncAcl`.
//!
//! `AclDefinition` provides a unified, flat list of `AclEntry` values
//! converted from the separate standard-entry fields and ida_entries
//! list of the wire-level `RsyncAcl` structure.
//!
//! # Upstream Reference
//!
//! Corresponds to the parsed result of `recv_rsync_acl()` in
//! `acls.c` lines 731-800.

use super::types::{AclEntry, AclPerms, AclTag};
use crate::acl::entry::RsyncAcl;

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
    pub(crate) entries: Vec<AclEntry>,
    /// Whether an explicit mask entry was present on the wire.
    ///
    /// When false and named entries exist, the mask was computed from
    /// the union of all named entry permissions.
    pub(crate) mask_set: bool,
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
