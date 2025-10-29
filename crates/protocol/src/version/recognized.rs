//! Helper set for collecting peer-advertised protocol versions during
//! negotiation.

use std::vec::Vec;

use super::SUPPORTED_PROTOCOL_COUNT;

/// Sorted set that records protocol versions recognised during negotiation.
#[derive(Clone)]
pub(crate) struct RecognizedVersions {
    values: [u8; SUPPORTED_PROTOCOL_COUNT],
    len: usize,
}

impl RecognizedVersions {
    /// Creates an empty set backed by a fixed-size buffer.
    pub(crate) const fn new() -> Self {
        Self {
            values: [0; SUPPORTED_PROTOCOL_COUNT],
            len: 0,
        }
    }

    /// Inserts a protocol version, preserving the sorted invariant.
    pub(crate) fn insert(&mut self, value: u8) {
        match self.values[..self.len].binary_search(&value) {
            Ok(_) => {}
            Err(index) => {
                if self.len == self.values.len() {
                    debug_assert!(false, "recognized protocol set exceeded capacity");
                    return;
                }

                if index == self.len {
                    self.values[self.len] = value;
                } else {
                    self.values.copy_within(index..self.len, index + 1);
                    self.values[index] = value;
                }

                self.len += 1;
            }
        }
    }

    /// Converts the collected versions into a sorted vector.
    pub(crate) fn into_vec(self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.len);
        result.extend_from_slice(&self.values[..self.len]);
        result
    }
}
