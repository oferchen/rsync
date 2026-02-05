//! Individual xattr entry representation.

use std::borrow::Cow;

use crate::xattr::MAX_FULL_DATUM;

/// State of an xattr entry during wire protocol exchange.
///
/// Used to track which xattr values need to be requested from the sender
/// after the initial abbreviated transmission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum XattrState {
    /// Value was abbreviated (checksum only) and needs full data.
    Abbrev = 1,
    /// Value has been fully received or doesn't need transfer.
    #[default]
    Done = 2,
    /// Value needs to be sent (marked by receiver's request).
    Todo = 3,
}

impl XattrState {
    /// Returns true if this entry needs its full value requested.
    pub const fn needs_request(&self) -> bool {
        matches!(self, XattrState::Abbrev)
    }

    /// Returns true if this entry needs its full value sent.
    pub const fn needs_send(&self) -> bool {
        matches!(self, XattrState::Todo)
    }
}

/// A single extended attribute with name, value, and transfer state.
#[derive(Debug, Clone)]
pub struct XattrEntry {
    /// Attribute name (e.g., "user.mime_type", "security.selinux").
    name: Vec<u8>,
    /// Attribute value or checksum if abbreviated.
    datum: Vec<u8>,
    /// Original datum length (differs from datum.len() if abbreviated).
    datum_len: usize,
    /// Transfer state for abbreviation protocol.
    state: XattrState,
}

impl XattrEntry {
    /// Creates a new xattr entry with full value.
    pub fn new(name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        let datum = value.into();
        let datum_len = datum.len();
        Self {
            name: name.into(),
            datum,
            datum_len,
            state: XattrState::Done,
        }
    }

    /// Creates an abbreviated xattr entry (checksum only).
    ///
    /// Used when receiving abbreviated xattr data from the wire.
    pub fn abbreviated(name: impl Into<Vec<u8>>, checksum: Vec<u8>, original_len: usize) -> Self {
        Self {
            name: name.into(),
            datum: checksum,
            datum_len: original_len,
            state: XattrState::Abbrev,
        }
    }

    /// Returns the attribute name.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Returns the attribute name as a string (lossy conversion).
    pub fn name_str(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }

    /// Returns the attribute value.
    ///
    /// If abbreviated, returns the checksum instead of the actual value.
    pub fn datum(&self) -> &[u8] {
        &self.datum
    }

    /// Returns the original datum length.
    ///
    /// For abbreviated entries, this is the full value length, not the checksum length.
    pub const fn datum_len(&self) -> usize {
        self.datum_len
    }

    /// Returns true if this entry is abbreviated (checksum only).
    pub const fn is_abbreviated(&self) -> bool {
        self.datum.len() != self.datum_len
    }

    /// Returns true if this entry's value should be abbreviated on the wire.
    ///
    /// Values larger than [`MAX_FULL_DATUM`] (32 bytes) are abbreviated.
    pub const fn should_abbreviate(&self) -> bool {
        self.datum_len > MAX_FULL_DATUM
    }

    /// Returns the transfer state.
    pub const fn state(&self) -> XattrState {
        self.state
    }

    /// Sets the transfer state.
    pub fn set_state(&mut self, state: XattrState) {
        self.state = state;
    }

    /// Marks this entry as needing its full value sent (XSTATE_TODO).
    pub fn mark_todo(&mut self) {
        self.state = XattrState::Todo;
    }

    /// Marks this entry as done (XSTATE_DONE).
    pub fn mark_done(&mut self) {
        self.state = XattrState::Done;
    }

    /// Updates this entry with the full value.
    ///
    /// Used when receiving the full value for an abbreviated entry.
    pub fn set_full_value(&mut self, value: Vec<u8>) {
        self.datum_len = value.len();
        self.datum = value;
        self.state = XattrState::Done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_entry_is_done() {
        let entry = XattrEntry::new("user.test", b"value".to_vec());
        assert_eq!(entry.state(), XattrState::Done);
        assert!(!entry.is_abbreviated());
    }

    #[test]
    fn abbreviated_entry_needs_request() {
        let checksum = vec![0u8; 16];
        let entry = XattrEntry::abbreviated("user.test", checksum, 100);
        assert_eq!(entry.state(), XattrState::Abbrev);
        assert!(entry.is_abbreviated());
        assert!(entry.state().needs_request());
        assert_eq!(entry.datum_len(), 100);
    }

    #[test]
    fn should_abbreviate_large_values() {
        let small = XattrEntry::new("user.small", vec![0u8; 32]);
        let large = XattrEntry::new("user.large", vec![0u8; 33]);

        assert!(!small.should_abbreviate());
        assert!(large.should_abbreviate());
    }

    #[test]
    fn set_full_value_clears_abbreviation() {
        let checksum = vec![0u8; 16];
        let mut entry = XattrEntry::abbreviated("user.test", checksum, 100);

        let full_value = vec![1u8; 100];
        entry.set_full_value(full_value.clone());

        assert!(!entry.is_abbreviated());
        assert_eq!(entry.datum(), &full_value);
        assert_eq!(entry.state(), XattrState::Done);
    }

    // ==================== Additional Comprehensive Tests ====================

    #[test]
    fn xattr_state_needs_request_only_for_abbrev() {
        assert!(XattrState::Abbrev.needs_request());
        assert!(!XattrState::Done.needs_request());
        assert!(!XattrState::Todo.needs_request());
    }

    #[test]
    fn xattr_state_needs_send_only_for_todo() {
        assert!(!XattrState::Abbrev.needs_send());
        assert!(!XattrState::Done.needs_send());
        assert!(XattrState::Todo.needs_send());
    }

    #[test]
    fn entry_name_accessor() {
        let entry = XattrEntry::new("user.test_name", b"value".to_vec());
        assert_eq!(entry.name(), b"user.test_name");
    }

    #[test]
    fn entry_name_str_conversion() {
        let entry = XattrEntry::new("user.test", b"value".to_vec());
        assert_eq!(entry.name_str(), "user.test");
    }

    #[test]
    fn entry_datum_accessor() {
        let value = b"test_value".to_vec();
        let entry = XattrEntry::new("user.test", value.clone());
        assert_eq!(entry.datum(), &value);
    }

    #[test]
    fn entry_datum_len_matches_value_length() {
        let value = vec![0u8; 50];
        let entry = XattrEntry::new("user.test", value.clone());
        assert_eq!(entry.datum_len(), 50);
        assert_eq!(entry.datum().len(), entry.datum_len());
    }

    #[test]
    fn abbreviated_entry_datum_len_differs_from_checksum_len() {
        let checksum = vec![0u8; 16]; // MD5 checksum length
        let entry = XattrEntry::abbreviated("user.test", checksum.clone(), 1000);

        assert_eq!(entry.datum_len(), 1000);
        assert_eq!(entry.datum().len(), 16);
        assert!(entry.is_abbreviated());
    }

    #[test]
    fn mark_todo_changes_state() {
        let mut entry = XattrEntry::new("user.test", b"value".to_vec());
        assert_eq!(entry.state(), XattrState::Done);

        entry.mark_todo();
        assert_eq!(entry.state(), XattrState::Todo);
        assert!(entry.state().needs_send());
    }

    #[test]
    fn mark_done_changes_state() {
        let checksum = vec![0u8; 16];
        let mut entry = XattrEntry::abbreviated("user.test", checksum, 100);
        assert_eq!(entry.state(), XattrState::Abbrev);

        entry.mark_done();
        assert_eq!(entry.state(), XattrState::Done);
    }

    #[test]
    fn set_state_directly() {
        let mut entry = XattrEntry::new("user.test", b"value".to_vec());

        entry.set_state(XattrState::Abbrev);
        assert_eq!(entry.state(), XattrState::Abbrev);

        entry.set_state(XattrState::Todo);
        assert_eq!(entry.state(), XattrState::Todo);

        entry.set_state(XattrState::Done);
        assert_eq!(entry.state(), XattrState::Done);
    }

    #[test]
    fn should_abbreviate_boundary_values() {
        // Exactly at MAX_FULL_DATUM (32)
        let at_boundary = XattrEntry::new("user.at", vec![0u8; MAX_FULL_DATUM]);
        assert!(!at_boundary.should_abbreviate());

        // One byte over
        let over_boundary = XattrEntry::new("user.over", vec![0u8; MAX_FULL_DATUM + 1]);
        assert!(over_boundary.should_abbreviate());
    }

    #[test]
    fn empty_value_not_abbreviated() {
        let entry = XattrEntry::new("user.empty", vec![]);
        assert!(!entry.should_abbreviate());
        assert!(!entry.is_abbreviated());
        assert_eq!(entry.datum_len(), 0);
    }

    #[test]
    fn set_full_value_updates_datum_len() {
        let checksum = vec![0u8; 16];
        let mut entry = XattrEntry::abbreviated("user.test", checksum, 100);
        assert_eq!(entry.datum_len(), 100);

        let new_value = vec![1u8; 200];
        entry.set_full_value(new_value.clone());

        assert_eq!(entry.datum_len(), 200);
        assert_eq!(entry.datum(), &new_value);
    }

    #[test]
    fn entry_with_binary_name() {
        let binary_name = vec![b'u', b's', b'e', b'r', b'.', 0xFF, 0xFE];
        let entry = XattrEntry::new(binary_name.clone(), b"value".to_vec());
        assert_eq!(entry.name(), &binary_name);
    }

    #[test]
    fn entry_with_binary_value() {
        let binary_value = vec![0x00, 0x01, 0xFF, 0xFE, 0x00];
        let entry = XattrEntry::new("user.binary", binary_value.clone());
        assert_eq!(entry.datum(), &binary_value);
    }

    #[test]
    fn entry_with_large_value() {
        let large_value = vec![0xABu8; 64 * 1024]; // 64KB
        let entry = XattrEntry::new("user.large", large_value.clone());

        assert!(entry.should_abbreviate());
        assert_eq!(entry.datum_len(), 64 * 1024);
        assert_eq!(entry.datum(), &large_value);
    }

    #[test]
    fn xattr_state_default_is_done() {
        let default_state = XattrState::default();
        assert_eq!(default_state, XattrState::Done);
    }
}
