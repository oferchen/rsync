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

    // ==================== Additional Comprehensive Tests ====================

    #[test]
    fn list_default_is_empty() {
        let list = XattrList::default();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn with_entries_constructor() {
        let entries = vec![
            XattrEntry::new("user.a", b"a".to_vec()),
            XattrEntry::new("user.b", b"b".to_vec()),
        ];
        let list = XattrList::with_entries(entries);

        assert_eq!(list.len(), 2);
        assert!(!list.is_empty());
    }

    #[test]
    fn entries_accessor() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"value".to_vec()));

        let entries = list.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), b"user.test");
    }

    #[test]
    fn entries_mut_accessor() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"old".to_vec()));

        let entries = list.entries_mut();
        entries[0].set_full_value(b"new".to_vec());

        assert_eq!(list.entries()[0].datum(), b"new");
    }

    #[test]
    fn iter_mut() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        for entry in list.iter_mut() {
            entry.mark_todo();
        }

        assert!(list.entries()[0].state().needs_send());
        assert!(list.entries()[1].state().needs_send());
    }

    #[test]
    fn has_abbreviated_returns_false_for_all_full() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"value_a".to_vec()));
        list.push(XattrEntry::new("user.b", b"value_b".to_vec()));

        assert!(!list.has_abbreviated());
    }

    #[test]
    fn abbreviated_indices_multiple() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.abbrev1", vec![0u8; 16], 100)); // 0
        list.push(XattrEntry::new("user.full1", b"small".to_vec())); // 1
        list.push(XattrEntry::abbreviated("user.abbrev2", vec![0u8; 16], 200)); // 2
        list.push(XattrEntry::new("user.full2", b"small".to_vec())); // 3
        list.push(XattrEntry::abbreviated("user.abbrev3", vec![0u8; 16], 300)); // 4

        let indices = list.abbreviated_indices();
        assert_eq!(indices, vec![0, 2, 4]);
    }

    #[test]
    fn todo_indices() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));
        list.push(XattrEntry::new("user.c", b"c".to_vec()));

        list.entries_mut()[0].mark_todo();
        list.entries_mut()[2].mark_todo();

        let indices = list.todo_indices();
        assert_eq!(indices, vec![0, 2]);
    }

    #[test]
    fn mark_todo_by_index() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        list.mark_todo(1);

        assert!(!list.entries()[0].state().needs_send());
        assert!(list.entries()[1].state().needs_send());
    }

    #[test]
    fn mark_todo_out_of_bounds_is_noop() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"value".to_vec()));

        // Should not panic, just do nothing
        list.mark_todo(100);

        assert_eq!(list.len(), 1);
    }

    #[test]
    fn set_full_value_by_index() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.test", vec![0u8; 16], 100));

        list.set_full_value(0, vec![1u8; 100]);

        assert!(!list.entries()[0].is_abbreviated());
        assert_eq!(list.entries()[0].datum(), &vec![1u8; 100]);
    }

    #[test]
    fn set_full_value_out_of_bounds_is_noop() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"value".to_vec()));

        // Should not panic, just do nothing
        list.set_full_value(100, vec![1u8; 50]);

        assert_eq!(list.len(), 1);
    }

    #[test]
    fn find_index_by_name() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.first", b"first".to_vec()));
        list.push(XattrEntry::new("user.second", b"second".to_vec()));
        list.push(XattrEntry::new("user.third", b"third".to_vec()));

        assert_eq!(list.find_index_by_name(b"user.first"), Some(0));
        assert_eq!(list.find_index_by_name(b"user.second"), Some(1));
        assert_eq!(list.find_index_by_name(b"user.third"), Some(2));
        assert_eq!(list.find_index_by_name(b"user.missing"), None);
    }

    #[test]
    fn into_iterator() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        let collected: Vec<_> = list.into_iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn ref_into_iterator() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        let names: Vec<_> = (&list).into_iter().map(|e| e.name_str()).collect();
        assert_eq!(names, vec!["user.a", "user.b"]);
    }

    #[test]
    fn from_iterator() {
        let entries = vec![
            XattrEntry::new("user.x", b"x".to_vec()),
            XattrEntry::new("user.y", b"y".to_vec()),
        ];

        let list: XattrList = entries.into_iter().collect();

        assert_eq!(list.len(), 2);
        assert_eq!(list.entries()[0].name(), b"user.x");
        assert_eq!(list.entries()[1].name(), b"user.y");
    }

    #[test]
    fn many_entries() {
        let mut list = XattrList::new();
        for i in 0..100 {
            list.push(XattrEntry::new(
                format!("user.attr{}", i),
                format!("value{}", i).into_bytes(),
            ));
        }

        assert_eq!(list.len(), 100);
        assert!(!list.is_empty());

        // Verify first and last
        assert_eq!(list.entries()[0].name(), b"user.attr0");
        assert_eq!(list.entries()[99].name(), b"user.attr99");
    }

    #[test]
    fn find_by_name_returns_correct_entry() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.first", b"first_value".to_vec()));
        list.push(XattrEntry::new("user.second", b"second_value".to_vec()));

        let found = list.find_by_name(b"user.second");
        assert!(found.is_some());
        let entry = found.unwrap();
        assert_eq!(entry.datum(), b"second_value");
    }
}
