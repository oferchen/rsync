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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_empty_set() {
        let set = RecognizedVersions::new();
        assert!(set.into_vec().is_empty());
    }

    #[test]
    fn insert_single_value() {
        let mut set = RecognizedVersions::new();
        set.insert(30);
        assert_eq!(set.into_vec(), vec![30]);
    }

    #[test]
    fn insert_maintains_sorted_order() {
        let mut set = RecognizedVersions::new();
        set.insert(31);
        set.insert(29);
        set.insert(30);
        assert_eq!(set.into_vec(), vec![29, 30, 31]);
    }

    #[test]
    fn insert_ignores_duplicates() {
        let mut set = RecognizedVersions::new();
        set.insert(30);
        set.insert(30);
        set.insert(30);
        assert_eq!(set.into_vec(), vec![30]);
    }

    #[test]
    fn insert_multiple_unique() {
        let mut set = RecognizedVersions::new();
        set.insert(28);
        set.insert(29);
        set.insert(30);
        set.insert(31);
        assert_eq!(set.into_vec(), vec![28, 29, 30, 31]);
    }

    #[test]
    fn into_vec_creates_owned_vector() {
        let mut set = RecognizedVersions::new();
        set.insert(30);
        let vec = set.into_vec();
        assert_eq!(vec.len(), 1);
        assert_eq!(vec[0], 30);
    }

    #[test]
    fn clone_creates_independent_copy() {
        let mut set = RecognizedVersions::new();
        set.insert(30);
        let cloned = set.clone();
        assert_eq!(cloned.into_vec(), vec![30]);
    }
}
