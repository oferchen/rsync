//! Xattr list management for wire protocol.

use crate::xattr::{XattrEntry, XattrState};

/// A collection of extended attributes for a single file.
///
/// Manages the xattr entries and their transfer states during the wire
/// protocol exchange. Supports the abbreviation protocol where large
/// values are transmitted as checksums and requested on-demand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XattrList {
    /// The xattr entries.
    entries: Vec<XattrEntry>,
}

impl XattrList {
    /// Creates an empty xattr list.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates an xattr list with the given entries.
    #[must_use]
    pub fn with_entries(entries: Vec<XattrEntry>) -> Self {
        Self { entries }
    }

    /// Number of extended attributes in this list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a slice of all entries.
    #[must_use]
    pub fn entries(&self) -> &[XattrEntry] {
        &self.entries
    }

    /// Returns a mutable slice of all entries.
    #[must_use]
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
    #[must_use]
    pub fn has_abbreviated(&self) -> bool {
        self.entries.iter().any(|e| e.state() == XattrState::Abbrev)
    }

    /// Returns indices of entries that need their full values requested.
    #[must_use]
    pub fn abbreviated_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.state().needs_request())
            .map(|(i, _)| i)
            .collect()
    }

    /// Returns indices of entries marked for sending (XSTATE_TODO).
    #[must_use]
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

    /// Sorts entries by name.
    ///
    /// Maintains the invariant that xattr lists are sorted alphabetically
    /// by name, matching upstream rsync's `qsort(rxa, count, sizeof(rsync_xa),
    /// rsync_xal_compare_names)` in `xattrs.c:863`.
    pub fn sort_by_name(&mut self) {
        self.entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));
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

    // --- Constructor tests ---

    #[test]
    fn new_creates_empty_list() {
        let list = XattrList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert!(!list.has_abbreviated());
        assert!(list.entries().is_empty());
    }

    #[test]
    fn default_creates_empty_list() {
        let list = XattrList::default();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn new_and_default_are_equal() {
        assert_eq!(XattrList::new(), XattrList::default());
    }

    #[test]
    fn with_entries_empty_vec() {
        let list = XattrList::with_entries(vec![]);
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn with_entries_single() {
        let entries = vec![XattrEntry::new("user.a", b"a".to_vec())];
        let list = XattrList::with_entries(entries);
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());
        assert_eq!(list.entries()[0].name(), b"user.a");
    }

    #[test]
    fn with_entries_multiple() {
        let entries = vec![
            XattrEntry::new("user.a", b"a".to_vec()),
            XattrEntry::new("user.b", b"b".to_vec()),
        ];
        let list = XattrList::with_entries(entries);
        assert_eq!(list.len(), 2);
    }

    // --- Push and len/is_empty ---

    #[test]
    fn push_increments_len() {
        let mut list = XattrList::new();
        assert_eq!(list.len(), 0);

        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());

        list.push(XattrEntry::new("user.b", b"b".to_vec()));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn push_preserves_insertion_order() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.c", b"3".to_vec()));
        list.push(XattrEntry::new("user.a", b"1".to_vec()));
        list.push(XattrEntry::new("user.b", b"2".to_vec()));

        assert_eq!(list.entries()[0].name(), b"user.c");
        assert_eq!(list.entries()[1].name(), b"user.a");
        assert_eq!(list.entries()[2].name(), b"user.b");
    }

    // --- Entries accessors ---

    #[test]
    fn entries_returns_all_entries() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.x", b"x".to_vec()));
        list.push(XattrEntry::new("user.y", b"y".to_vec()));

        let entries = list.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name(), b"user.x");
        assert_eq!(entries[1].name(), b"user.y");
    }

    #[test]
    fn entries_empty_list() {
        let list = XattrList::new();
        assert!(list.entries().is_empty());
    }

    #[test]
    fn entries_mut_allows_modification() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"old".to_vec()));

        list.entries_mut()[0].set_full_value(b"new".to_vec());

        assert_eq!(list.entries()[0].datum(), b"new");
    }

    #[test]
    fn entries_mut_empty_list() {
        let mut list = XattrList::new();
        assert!(list.entries_mut().is_empty());
    }

    // --- Iterators ---

    #[test]
    fn iter_yields_all_entries_in_order() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"va".to_vec()));
        list.push(XattrEntry::new("user.b", b"vb".to_vec()));

        let names: Vec<_> = list.iter().map(|e| e.name_str().into_owned()).collect();
        assert_eq!(names, vec!["user.a", "user.b"]);
    }

    #[test]
    fn iter_empty_list() {
        let list = XattrList::new();
        assert_eq!(list.iter().count(), 0);
    }

    #[test]
    fn iter_mut_modifies_all_entries() {
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
    fn iter_mut_empty_list() {
        let mut list = XattrList::new();
        assert_eq!(list.iter_mut().count(), 0);
    }

    #[test]
    fn into_iter_owned_consumes_list() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        let collected: Vec<XattrEntry> = list.into_iter().collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].name(), b"user.a");
        assert_eq!(collected[1].name(), b"user.b");
    }

    #[test]
    fn into_iter_owned_empty() {
        let list = XattrList::new();
        let collected: Vec<XattrEntry> = list.into_iter().collect();
        assert!(collected.is_empty());
    }

    #[test]
    fn into_iter_ref_borrows_list() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        let names: Vec<_> = (&list)
            .into_iter()
            .map(|e| e.name_str().into_owned())
            .collect();
        assert_eq!(names, vec!["user.a", "user.b"]);
        // list still accessible after ref iteration
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn into_iter_ref_empty() {
        let list = XattrList::new();
        let count = (&list).into_iter().count();
        assert_eq!(count, 0);
    }

    #[test]
    fn for_loop_uses_ref_into_iter() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        let mut count = 0;
        for _entry in &list {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    // --- FromIterator ---

    #[test]
    fn from_iterator_collects_entries() {
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
    fn from_iterator_empty() {
        let list: XattrList = std::iter::empty::<XattrEntry>().collect();
        assert!(list.is_empty());
    }

    #[test]
    fn from_iterator_single() {
        let list: XattrList =
            std::iter::once(XattrEntry::new("user.only", b"v".to_vec())).collect();
        assert_eq!(list.len(), 1);
        assert_eq!(list.entries()[0].name(), b"user.only");
    }

    // --- Abbreviated tracking ---

    #[test]
    fn has_abbreviated_empty_list() {
        let list = XattrList::new();
        assert!(!list.has_abbreviated());
    }

    #[test]
    fn has_abbreviated_all_full() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"value_a".to_vec()));
        list.push(XattrEntry::new("user.b", b"value_b".to_vec()));
        assert!(!list.has_abbreviated());
    }

    #[test]
    fn has_abbreviated_one_abbreviated() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.full", b"small".to_vec()));
        list.push(XattrEntry::abbreviated("user.abbrev", vec![0u8; 16], 100));
        assert!(list.has_abbreviated());
    }

    #[test]
    fn has_abbreviated_all_abbreviated() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.a", vec![0u8; 16], 50));
        list.push(XattrEntry::abbreviated("user.b", vec![1u8; 16], 80));
        assert!(list.has_abbreviated());
    }

    #[test]
    fn has_abbreviated_after_set_full_value() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.abbrev", vec![0u8; 16], 100));
        assert!(list.has_abbreviated());

        list.set_full_value(0, vec![1u8; 100]);
        assert!(!list.has_abbreviated());
    }

    #[test]
    fn abbreviated_indices_empty_list() {
        let list = XattrList::new();
        assert!(list.abbreviated_indices().is_empty());
    }

    #[test]
    fn abbreviated_indices_none_abbreviated() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));
        assert!(list.abbreviated_indices().is_empty());
    }

    #[test]
    fn abbreviated_indices_mixed() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.a", vec![0u8; 16], 100));
        list.push(XattrEntry::new("user.b", b"small".to_vec()));
        list.push(XattrEntry::abbreviated("user.c", vec![0u8; 16], 200));
        list.push(XattrEntry::new("user.d", b"small".to_vec()));
        list.push(XattrEntry::abbreviated("user.e", vec![0u8; 16], 300));

        assert_eq!(list.abbreviated_indices(), vec![0, 2, 4]);
    }

    #[test]
    fn abbreviated_indices_all_abbreviated() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.a", vec![0u8; 16], 50));
        list.push(XattrEntry::abbreviated("user.b", vec![0u8; 16], 80));

        assert_eq!(list.abbreviated_indices(), vec![0, 1]);
    }

    // --- Todo tracking ---

    #[test]
    fn todo_indices_empty_list() {
        let list = XattrList::new();
        assert!(list.todo_indices().is_empty());
    }

    #[test]
    fn todo_indices_none_todo() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));
        assert!(list.todo_indices().is_empty());
    }

    #[test]
    fn todo_indices_selective() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));
        list.push(XattrEntry::new("user.c", b"c".to_vec()));

        list.mark_todo(0);
        list.mark_todo(2);

        assert_eq!(list.todo_indices(), vec![0, 2]);
    }

    #[test]
    fn todo_indices_all_todo() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        list.mark_todo(0);
        list.mark_todo(1);

        assert_eq!(list.todo_indices(), vec![0, 1]);
    }

    // --- mark_todo ---

    #[test]
    fn mark_todo_first_index() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));

        list.mark_todo(0);

        assert!(list.entries()[0].state().needs_send());
        assert!(!list.entries()[1].state().needs_send());
    }

    #[test]
    fn mark_todo_last_index() {
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

        list.mark_todo(1);
        list.mark_todo(100);
        list.mark_todo(usize::MAX);

        assert_eq!(list.len(), 1);
        assert!(!list.entries()[0].state().needs_send());
    }

    #[test]
    fn mark_todo_on_empty_list_is_noop() {
        let mut list = XattrList::new();
        list.mark_todo(0);
        assert!(list.is_empty());
    }

    #[test]
    fn mark_todo_idempotent() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));

        list.mark_todo(0);
        list.mark_todo(0);

        assert!(list.entries()[0].state().needs_send());
        assert_eq!(list.todo_indices(), vec![0]);
    }

    // --- set_full_value ---

    #[test]
    fn set_full_value_resolves_abbreviation() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.test", vec![0u8; 16], 100));

        assert!(list.has_abbreviated());

        list.set_full_value(0, vec![1u8; 100]);

        assert!(!list.entries()[0].is_abbreviated());
        assert_eq!(list.entries()[0].datum(), &vec![1u8; 100]);
        assert_eq!(list.entries()[0].state(), XattrState::Done);
        assert!(!list.has_abbreviated());
    }

    #[test]
    fn set_full_value_out_of_bounds_is_noop() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"value".to_vec()));

        list.set_full_value(1, vec![1u8; 50]);
        list.set_full_value(100, vec![1u8; 50]);

        assert_eq!(list.entries()[0].datum(), b"value");
    }

    #[test]
    fn set_full_value_on_empty_list_is_noop() {
        let mut list = XattrList::new();
        list.set_full_value(0, vec![1u8; 50]);
        assert!(list.is_empty());
    }

    #[test]
    fn set_full_value_on_non_abbreviated_entry() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"old".to_vec()));

        list.set_full_value(0, b"new_value".to_vec());

        assert_eq!(list.entries()[0].datum(), b"new_value");
        assert_eq!(list.entries()[0].state(), XattrState::Done);
    }

    #[test]
    fn set_full_value_partial_resolution() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.a", vec![0u8; 16], 50));
        list.push(XattrEntry::abbreviated("user.b", vec![0u8; 16], 80));

        assert_eq!(list.abbreviated_indices(), vec![0, 1]);

        list.set_full_value(0, vec![1u8; 50]);

        assert_eq!(list.abbreviated_indices(), vec![1]);
        assert!(list.has_abbreviated());
    }

    // --- sort_by_name ---

    #[test]
    fn sort_by_name_empty_list() {
        let mut list = XattrList::new();
        list.sort_by_name();
        assert!(list.is_empty());
    }

    #[test]
    fn sort_by_name_single_entry() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.only", b"val".to_vec()));
        list.sort_by_name();
        assert_eq!(list.len(), 1);
        assert_eq!(list.entries()[0].name(), b"user.only");
    }

    #[test]
    fn sort_by_name_reverse_order() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.zebra", b"z".to_vec()));
        list.push(XattrEntry::new("user.alpha", b"a".to_vec()));
        list.push(XattrEntry::new("user.middle", b"m".to_vec()));

        list.sort_by_name();

        assert_eq!(list.entries()[0].name(), b"user.alpha");
        assert_eq!(list.entries()[1].name(), b"user.middle");
        assert_eq!(list.entries()[2].name(), b"user.zebra");
    }

    #[test]
    fn sort_by_name_already_sorted() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"1".to_vec()));
        list.push(XattrEntry::new("user.b", b"2".to_vec()));
        list.push(XattrEntry::new("user.c", b"3".to_vec()));

        list.sort_by_name();

        assert_eq!(list.entries()[0].name(), b"user.a");
        assert_eq!(list.entries()[1].name(), b"user.b");
        assert_eq!(list.entries()[2].name(), b"user.c");
    }

    #[test]
    fn sort_by_name_preserves_values() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.z", b"zval".to_vec()));
        list.push(XattrEntry::new("user.a", b"aval".to_vec()));

        list.sort_by_name();

        assert_eq!(list.entries()[0].name(), b"user.a");
        assert_eq!(list.entries()[0].datum(), b"aval");
        assert_eq!(list.entries()[1].name(), b"user.z");
        assert_eq!(list.entries()[1].datum(), b"zval");
    }

    #[test]
    fn sort_by_name_with_common_prefixes() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.abc", b"1".to_vec()));
        list.push(XattrEntry::new("user.ab", b"2".to_vec()));
        list.push(XattrEntry::new("user.a", b"3".to_vec()));

        list.sort_by_name();

        assert_eq!(list.entries()[0].name(), b"user.a");
        assert_eq!(list.entries()[1].name(), b"user.ab");
        assert_eq!(list.entries()[2].name(), b"user.abc");
    }

    #[test]
    fn sort_by_name_binary_names() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new(vec![0xFF, 0xFE], b"high".to_vec()));
        list.push(XattrEntry::new(vec![0x00, 0x01], b"low".to_vec()));
        list.push(XattrEntry::new(vec![0x80], b"mid".to_vec()));

        list.sort_by_name();

        assert_eq!(list.entries()[0].name(), &[0x00, 0x01]);
        assert_eq!(list.entries()[1].name(), &[0x80]);
        assert_eq!(list.entries()[2].name(), &[0xFF, 0xFE]);
    }

    #[test]
    fn sort_by_name_mixed_namespaces() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.z", b"1".to_vec()));
        list.push(XattrEntry::new("security.selinux", b"2".to_vec()));
        list.push(XattrEntry::new("system.posix_acl_access", b"3".to_vec()));

        list.sort_by_name();

        assert_eq!(list.entries()[0].name(), b"security.selinux");
        assert_eq!(list.entries()[1].name(), b"system.posix_acl_access");
        assert_eq!(list.entries()[2].name(), b"user.z");
    }

    // --- find_by_name ---

    #[test]
    fn find_by_name_exists() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.first", b"first_value".to_vec()));
        list.push(XattrEntry::new("user.second", b"second_value".to_vec()));

        let found = list.find_by_name(b"user.second").unwrap();
        assert_eq!(found.datum(), b"second_value");
    }

    #[test]
    fn find_by_name_missing() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"value".to_vec()));
        assert!(list.find_by_name(b"user.missing").is_none());
    }

    #[test]
    fn find_by_name_empty_list() {
        let list = XattrList::new();
        assert!(list.find_by_name(b"user.anything").is_none());
    }

    #[test]
    fn find_by_name_returns_first_match() {
        let list = XattrList::with_entries(vec![
            XattrEntry::new("user.dup", b"first".to_vec()),
            XattrEntry::new("user.dup", b"second".to_vec()),
        ]);

        let found = list.find_by_name(b"user.dup").unwrap();
        assert_eq!(found.datum(), b"first");
    }

    #[test]
    fn find_by_name_binary_name() {
        let binary_name = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut list = XattrList::new();
        list.push(XattrEntry::new(binary_name.clone(), b"binary".to_vec()));

        assert!(list.find_by_name(&binary_name).is_some());
        assert!(list.find_by_name(&[0xDE, 0xAD]).is_none());
    }

    #[test]
    fn find_by_name_empty_name() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new(vec![], b"empty_name".to_vec()));

        assert!(list.find_by_name(b"").is_some());
        assert!(list.find_by_name(b"notempty").is_none());
    }

    // --- find_index_by_name ---

    #[test]
    fn find_index_by_name_all_positions() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.first", b"1".to_vec()));
        list.push(XattrEntry::new("user.second", b"2".to_vec()));
        list.push(XattrEntry::new("user.third", b"3".to_vec()));

        assert_eq!(list.find_index_by_name(b"user.first"), Some(0));
        assert_eq!(list.find_index_by_name(b"user.second"), Some(1));
        assert_eq!(list.find_index_by_name(b"user.third"), Some(2));
    }

    #[test]
    fn find_index_by_name_missing() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.test", b"v".to_vec()));
        assert_eq!(list.find_index_by_name(b"user.missing"), None);
    }

    #[test]
    fn find_index_by_name_empty_list() {
        let list = XattrList::new();
        assert_eq!(list.find_index_by_name(b"user.anything"), None);
    }

    #[test]
    fn find_index_by_name_duplicates_returns_first() {
        let list = XattrList::with_entries(vec![
            XattrEntry::new("user.dup", b"first".to_vec()),
            XattrEntry::new("user.other", b"mid".to_vec()),
            XattrEntry::new("user.dup", b"second".to_vec()),
        ]);

        assert_eq!(list.find_index_by_name(b"user.dup"), Some(0));
    }

    // --- Clone / PartialEq / Debug ---

    #[test]
    fn clone_produces_equal_list() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"val".to_vec()));
        list.push(XattrEntry::abbreviated("user.b", vec![0u8; 16], 200));

        let cloned = list.clone();
        assert_eq!(list, cloned);
    }

    #[test]
    fn equality_different_entries() {
        let list_a = XattrList::with_entries(vec![XattrEntry::new("user.a", b"a".to_vec())]);
        let list_b = XattrList::with_entries(vec![XattrEntry::new("user.b", b"b".to_vec())]);
        assert_ne!(list_a, list_b);
    }

    #[test]
    fn equality_different_lengths() {
        let list_a = XattrList::with_entries(vec![XattrEntry::new("user.a", b"a".to_vec())]);
        let list_b = XattrList::with_entries(vec![
            XattrEntry::new("user.a", b"a".to_vec()),
            XattrEntry::new("user.b", b"b".to_vec()),
        ]);
        assert_ne!(list_a, list_b);
    }

    #[test]
    fn debug_format_is_readable() {
        let list = XattrList::new();
        let debug = format!("{list:?}");
        assert!(debug.contains("XattrList"));
    }

    // --- Many entries ---

    #[test]
    fn many_entries_push_and_access() {
        let mut list = XattrList::new();
        for i in 0..100 {
            list.push(XattrEntry::new(
                format!("user.attr{i}"),
                format!("value{i}").into_bytes(),
            ));
        }

        assert_eq!(list.len(), 100);
        assert_eq!(list.entries()[0].name(), b"user.attr0");
        assert_eq!(list.entries()[99].name(), b"user.attr99");
    }

    #[test]
    fn many_entries_sort_and_find() {
        let mut list = XattrList::new();
        for i in (0..50).rev() {
            list.push(XattrEntry::new(
                format!("user.attr{i:03}"),
                format!("v{i}").into_bytes(),
            ));
        }

        list.sort_by_name();

        assert_eq!(list.entries()[0].name(), b"user.attr000");
        assert_eq!(list.entries()[49].name(), b"user.attr049");

        assert_eq!(list.find_index_by_name(b"user.attr025"), Some(25));
    }

    // --- Combined state transitions ---

    #[test]
    fn mark_todo_then_check_todo_indices() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.a", b"a".to_vec()));
        list.push(XattrEntry::new("user.b", b"b".to_vec()));
        list.push(XattrEntry::new("user.c", b"c".to_vec()));

        assert!(list.todo_indices().is_empty());

        list.mark_todo(1);
        assert_eq!(list.todo_indices(), vec![1]);

        list.mark_todo(0);
        assert_eq!(list.todo_indices(), vec![0, 1]);
    }

    #[test]
    fn abbreviated_then_resolve_then_check() {
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated("user.a", vec![0u8; 16], 50));
        list.push(XattrEntry::abbreviated("user.b", vec![0u8; 16], 80));
        list.push(XattrEntry::new("user.c", b"full".to_vec()));

        assert!(list.has_abbreviated());
        assert_eq!(list.abbreviated_indices(), vec![0, 1]);

        // Resolve first abbreviated entry
        list.set_full_value(0, vec![1u8; 50]);
        assert!(list.has_abbreviated());
        assert_eq!(list.abbreviated_indices(), vec![1]);

        // Resolve second abbreviated entry
        list.set_full_value(1, vec![2u8; 80]);
        assert!(!list.has_abbreviated());
        assert!(list.abbreviated_indices().is_empty());
    }

    #[test]
    fn sort_preserves_state() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.z", b"z".to_vec()));
        list.push(XattrEntry::abbreviated("user.a", vec![0u8; 16], 100));

        list.mark_todo(0);

        list.sort_by_name();

        // After sorting, "user.a" (abbreviated) is first, "user.z" (todo) is second
        assert_eq!(list.entries()[0].name(), b"user.a");
        assert_eq!(list.entries()[0].state(), XattrState::Abbrev);
        assert_eq!(list.entries()[1].name(), b"user.z");
        assert!(list.entries()[1].state().needs_send());
    }

    #[test]
    fn find_after_sort() {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("user.z", b"zval".to_vec()));
        list.push(XattrEntry::new("user.a", b"aval".to_vec()));

        // Before sort, user.z is at index 0
        assert_eq!(list.find_index_by_name(b"user.z"), Some(0));

        list.sort_by_name();

        // After sort, user.z moves to index 1
        assert_eq!(list.find_index_by_name(b"user.z"), Some(1));
        assert_eq!(list.find_index_by_name(b"user.a"), Some(0));
    }
}
