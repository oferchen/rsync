//! Wire protocol types for extended attribute exchange.
//!
//! Defines the data structures produced by decoding xattr wire data
//! and consumed by encoding functions.

use crate::xattr::{XattrEntry, XattrList};

/// A single parsed xattr name-value pair from the wire.
///
/// Represents one extended attribute as received during file list transfer,
/// before cache resolution or name translation. The name is in wire format
/// (e.g., without `user.` prefix on Linux) and has the trailing NUL stripped.
///
/// For values larger than `MAX_FULL_DATUM` (32 bytes), the datum contains
/// only a checksum and `is_abbreviated()` returns true.
///
/// # Upstream Reference
///
/// Corresponds to one iteration of the entry loop in `xattrs.c:receive_xattr()`.
#[derive(Debug, Clone)]
pub struct XattrDefinition {
    /// Attribute name in wire format (NUL-stripped).
    pub(super) name: Vec<u8>,
    /// Full value bytes, or checksum bytes if abbreviated.
    pub(super) datum: Vec<u8>,
    /// Original value length on the sender side.
    pub(super) datum_len: usize,
    /// True when the value exceeds `MAX_FULL_DATUM` and only a checksum was sent.
    pub(super) abbreviated: bool,
}

impl XattrDefinition {
    /// Returns the attribute name (wire format, NUL-stripped).
    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Returns the attribute name as a lossy UTF-8 string.
    #[must_use]
    pub fn name_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }

    /// Returns the datum bytes - full value if small, checksum if abbreviated.
    #[must_use]
    pub fn datum(&self) -> &[u8] {
        &self.datum
    }

    /// Returns the original value length on the sender.
    ///
    /// For abbreviated entries this differs from `datum().len()`.
    #[must_use]
    pub const fn datum_len(&self) -> usize {
        self.datum_len
    }

    /// Returns true if this entry was abbreviated (checksum only, no full value).
    #[must_use]
    pub const fn is_abbreviated(&self) -> bool {
        self.abbreviated
    }

    /// Converts this definition into an `XattrEntry` for use with `XattrList`.
    #[must_use]
    pub fn into_entry(self) -> XattrEntry {
        if self.abbreviated {
            XattrEntry::abbreviated(self.name, self.datum, self.datum_len)
        } else {
            XattrEntry::new(self.name, self.datum)
        }
    }
}

/// A parsed set of xattr name-value pairs from the wire.
///
/// Contains zero or more `XattrDefinition` entries as read from a single
/// literal xattr block during file list transfer. Names are in wire format
/// and have not been translated to local platform conventions.
///
/// # Upstream Reference
///
/// Corresponds to the literal-data branch of `xattrs.c:receive_xattr()`,
/// after reading `ndx == 0` and before `rsync_xal_store()`.
#[derive(Debug, Clone, Default)]
pub struct XattrSet {
    /// Parsed entries in wire order.
    pub(super) entries: Vec<XattrDefinition>,
}

impl XattrSet {
    /// Creates an empty xattr set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a slice of all entries.
    #[must_use]
    pub fn entries(&self) -> &[XattrDefinition] {
        &self.entries
    }

    /// Consumes the set and returns the entries as a vector.
    #[must_use]
    pub fn into_entries(self) -> Vec<XattrDefinition> {
        self.entries
    }

    /// Converts this set into an `XattrList` for use with the cache and
    /// abbreviation protocol.
    #[must_use]
    pub fn into_xattr_list(self) -> XattrList {
        let entries: Vec<XattrEntry> = self
            .entries
            .into_iter()
            .map(XattrDefinition::into_entry)
            .collect();
        XattrList::with_entries(entries)
    }

    /// Returns an iterator over the entries.
    pub fn iter(&self) -> impl Iterator<Item = &XattrDefinition> {
        self.entries.iter()
    }
}

impl<'a> IntoIterator for &'a XattrSet {
    type Item = &'a XattrDefinition;
    type IntoIter = std::slice::Iter<'a, XattrDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl IntoIterator for XattrSet {
    type Item = XattrDefinition;
    type IntoIter = std::vec::IntoIter<XattrDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// Result of receiving xattr data from the wire.
#[derive(Debug)]
pub enum RecvXattrResult {
    /// A cache index was received - look up in the xattr cache.
    CacheHit(u32),
    /// Literal xattr data was received.
    Literal(XattrList),
}
