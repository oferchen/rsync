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
}
