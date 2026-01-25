//! Xattr list management for wire protocol.

use crate::xattr::{XattrEntry, XattrState};

/// A collection of extended attributes for a single file.
///
/// Manages the xattr entries and their transfer states during the wire
/// protocol exchange. Supports the abbreviation protocol where large
/// values are transmitted as checksums and requested on-demand.
#[derive(Debug, Clone, Default)]
pub struct XattrList {
    /// The xattr entries.
    entries: Vec<XattrEntry>,
}

impl XattrList {
    /// Creates an empty xattr list.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates an xattr list with the given entries.
    pub fn with_entries(entries: Vec<XattrEntry>) -> Self {
        Self { entries }
    }

    /// Returns the number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the list is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a slice of all entries.
    pub fn entries(&self) -> &[XattrEntry] {
        &self.entries
    }

    /// Returns a mutable slice of all entries.
    pub fn entries_mut(&mut self) -> &mut [XattrEntry] {
        &mut self.entries
    }

    /// Adds an entry to the list.
    pub fn push(&mut self, entry: XattrEntry) {
        self.entries.push(entry);
    }

    /// Returns an iterator over the entries.
    pub fn iter(&self) -> impl Iterator<Item = &XattrEntry> {
        self.entries.iter()
    }

    /// Returns a mutable iterator over the entries.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut XattrEntry> {
        self.entries.iter_mut()
    }

    /// Returns true if any entry is abbreviated and needs its full value.
    pub fn has_abbreviated(&self) -> bool {
        self.entries.iter().any(|e| e.state() == XattrState::Abbrev)
    }

    /// Returns indices of entries that need their full values requested.
    pub fn abbreviated_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.state().needs_request())
            .map(|(i, _)| i)
            .collect()
    }

    /// Returns indices of entries marked for sending (XSTATE_TODO).
    pub fn todo_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.state().needs_send())
            .map(|(i, _)| i)
            .collect()
    }

    /// Marks the entry at the given index as needing to be sent.
    pub fn mark_todo(&mut self, index: usize) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.mark_todo();
        }
    }

    /// Sets the full value for an entry at the given index.
    pub fn set_full_value(&mut self, index: usize, value: Vec<u8>) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.set_full_value(value);
        }
    }

    /// Finds an entry by name.
    pub fn find_by_name(&self, name: &[u8]) -> Option<&XattrEntry> {
        self.entries.iter().find(|e| e.name() == name)
    }

    /// Finds the index of an entry by name.
    pub fn find_index_by_name(&self, name: &[u8]) -> Option<usize> {
        self.entries.iter().position(|e| e.name() == name)
    }
}

impl IntoIterator for XattrList {
    type Item = XattrEntry;
    type IntoIter = std::vec::IntoIter<XattrEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a> IntoIterator for &'a XattrList {
    type Item = &'a XattrEntry;
    type IntoIter = std::slice::Iter<'a, XattrEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl FromIterator<XattrEntry> for XattrList {
    fn from_iter<T: IntoIterator<Item = XattrEntry>>(iter: T) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list() {
        let list = XattrList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert!(!list.has_abbreviated());
    }

    #[test]
    fn push_and_iterate() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"value_a".to_vec()));
        list.push(XattrEntry::new("user.b", b"value_b".to_vec()));

        assert_eq!(list.len(), 2);

        let names: Vec<_> = list.iter().map(|e| e.name_str()).collect();
        assert_eq!(names, vec!["user.a", "user.b"]);
    }

    #[test]
    fn abbreviated_tracking() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.full", b"small".to_vec()));
        list.push(XattrEntry::abbreviated("user.abbrev", vec![0u8; 16], 100));

        assert!(list.has_abbreviated());
        assert_eq!(list.abbreviated_indices(), vec![1]);
    }

    #[test]
    fn find_by_name() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"value".to_vec()));

        assert!(list.find_by_name(b"user.test").is_some());
        assert!(list.find_by_name(b"user.missing").is_none());
    }
}
