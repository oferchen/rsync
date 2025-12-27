//! Shared compression state for file list encoding and decoding.
//!
//! The rsync protocol compresses file list entries by omitting fields that
//! match the previous entry. This module provides a shared state structure
//! used by both [`FileListReader`] and [`FileListWriter`] to track the
//! previous entry's values.

/// Compression state for sequential file list processing.
///
/// Tracks the previous entry's metadata to enable compression/decompression
/// of repeated values across consecutive entries.
#[derive(Debug, Clone, Default)]
pub struct FileListCompressionState {
    /// Previous entry's path bytes (for name prefix compression).
    pub prev_name: Vec<u8>,
    /// Previous entry's file mode.
    pub prev_mode: u32,
    /// Previous entry's modification time.
    pub prev_mtime: i64,
    /// Previous entry's user ID.
    pub prev_uid: u32,
    /// Previous entry's group ID.
    pub prev_gid: u32,
}

impl FileListCompressionState {
    /// Creates a new compression state with default (zero) values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Calculates the common prefix length between the previous name and a new name.
    ///
    /// Returns the number of bytes that can be shared, capped at 255
    /// (the maximum value that fits in a single byte).
    #[must_use]
    pub fn calculate_name_prefix_len(&self, name: &[u8]) -> usize {
        self.prev_name
            .iter()
            .zip(name.iter())
            .take_while(|(a, b)| a == b)
            .count()
            .min(255)
    }

    /// Updates the state with values from a new entry.
    ///
    /// Call this after processing each entry to prepare for the next one.
    pub fn update(&mut self, name: &[u8], mode: u32, mtime: i64, uid: u32, gid: u32) {
        self.prev_name = name.to_vec();
        self.prev_mode = mode;
        self.prev_mtime = mtime;
        self.prev_uid = uid;
        self.prev_gid = gid;
    }

    /// Updates only the name portion of the state.
    pub fn update_name(&mut self, name: &[u8]) {
        self.prev_name = name.to_vec();
    }

    /// Updates only the mode portion of the state.
    pub fn update_mode(&mut self, mode: u32) {
        self.prev_mode = mode;
    }

    /// Updates only the mtime portion of the state.
    pub fn update_mtime(&mut self, mtime: i64) {
        self.prev_mtime = mtime;
    }

    /// Updates only the uid portion of the state.
    pub fn update_uid(&mut self, uid: u32) {
        self.prev_uid = uid;
    }

    /// Updates only the gid portion of the state.
    pub fn update_gid(&mut self, gid: u32) {
        self.prev_gid = gid;
    }

    /// Resets the compression state to initial values.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_has_default_values() {
        let state = FileListCompressionState::new();
        assert!(state.prev_name.is_empty());
        assert_eq!(state.prev_mode, 0);
        assert_eq!(state.prev_mtime, 0);
        assert_eq!(state.prev_uid, 0);
        assert_eq!(state.prev_gid, 0);
    }

    #[test]
    fn calculate_name_prefix_len_empty_prev() {
        let state = FileListCompressionState::new();
        assert_eq!(state.calculate_name_prefix_len(b"test.txt"), 0);
    }

    #[test]
    fn calculate_name_prefix_len_with_prefix() {
        let mut state = FileListCompressionState::new();
        state.prev_name = b"dir/file1.txt".to_vec();

        // "dir/file1.txt" vs "dir/file2.txt" - differs at position 8 ('1' vs '2')
        assert_eq!(state.calculate_name_prefix_len(b"dir/file2.txt"), 8); // "dir/file"
        assert_eq!(state.calculate_name_prefix_len(b"dir/other.txt"), 4); // "dir/"
        assert_eq!(state.calculate_name_prefix_len(b"other/file.txt"), 0);
    }

    #[test]
    fn calculate_name_prefix_len_caps_at_255() {
        let mut state = FileListCompressionState::new();
        state.prev_name = vec![b'a'; 300];

        let name = vec![b'a'; 300];
        assert_eq!(state.calculate_name_prefix_len(&name), 255);
    }

    #[test]
    fn calculate_name_prefix_len_full_match() {
        let mut state = FileListCompressionState::new();
        state.prev_name = b"exact_match".to_vec();

        assert_eq!(state.calculate_name_prefix_len(b"exact_match"), 11);
    }

    #[test]
    fn update_sets_all_fields() {
        let mut state = FileListCompressionState::new();
        state.update(b"test.txt", 0o644, 1700000000, 1000, 1000);

        assert_eq!(state.prev_name, b"test.txt");
        assert_eq!(state.prev_mode, 0o644);
        assert_eq!(state.prev_mtime, 1700000000);
        assert_eq!(state.prev_uid, 1000);
        assert_eq!(state.prev_gid, 1000);
    }

    #[test]
    fn update_individual_fields() {
        let mut state = FileListCompressionState::new();

        state.update_name(b"file.txt");
        assert_eq!(state.prev_name, b"file.txt");

        state.update_mode(0o755);
        assert_eq!(state.prev_mode, 0o755);

        state.update_mtime(1234567890);
        assert_eq!(state.prev_mtime, 1234567890);

        state.update_uid(500);
        assert_eq!(state.prev_uid, 500);

        state.update_gid(600);
        assert_eq!(state.prev_gid, 600);
    }

    #[test]
    fn reset_clears_all_fields() {
        let mut state = FileListCompressionState::new();
        state.update(b"test.txt", 0o644, 1700000000, 1000, 1000);

        state.reset();

        assert!(state.prev_name.is_empty());
        assert_eq!(state.prev_mode, 0);
        assert_eq!(state.prev_mtime, 0);
        assert_eq!(state.prev_uid, 0);
        assert_eq!(state.prev_gid, 0);
    }
}
