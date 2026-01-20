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
    prev_name: Vec<u8>,
    /// Previous entry's file mode.
    prev_mode: u32,
    /// Previous entry's modification time.
    prev_mtime: i64,
    /// Previous entry's access time (for XMIT_SAME_ATIME).
    prev_atime: i64,
    /// Previous entry's user ID.
    prev_uid: u32,
    /// Previous entry's group ID.
    prev_gid: u32,
    /// Previous entry's device major number (for XMIT_SAME_RDEV_MAJOR).
    prev_rdev_major: u32,
    /// Previous entry's device number (for XMIT_SAME_RDEV_pre28, protocols < 28).
    prev_rdev: u64,
    /// Previous hardlink device number (for XMIT_SAME_DEV_pre30, protocols 26-29).
    prev_hardlink_dev: i64,
}

/// Statistics collected during file list transmission/reception.
///
/// Tracks counts and sizes for progress reporting and verification.
#[derive(Debug, Clone, Default)]
pub struct FileListStats {
    /// Number of regular files processed.
    pub num_files: u64,
    /// Number of directories processed.
    pub num_dirs: u64,
    /// Number of symbolic links processed.
    pub num_symlinks: u64,
    /// Number of device files processed (block and character).
    pub num_devices: u64,
    /// Number of special files processed (FIFOs, sockets).
    pub num_specials: u64,
    /// Number of entries with ACLs.
    pub num_acls: u64,
    /// Number of entries with extended attributes.
    pub num_xattrs: u64,
    /// Total size of all regular files and symlinks in bytes.
    pub total_size: u64,
}

impl FileListCompressionState {
    /// Creates a new compression state with default (zero) values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the previous entry's name bytes.
    #[must_use]
    pub fn prev_name(&self) -> &[u8] {
        &self.prev_name
    }

    /// Returns the previous entry's mode.
    #[must_use]
    pub const fn prev_mode(&self) -> u32 {
        self.prev_mode
    }

    /// Returns the previous entry's mtime.
    #[must_use]
    pub const fn prev_mtime(&self) -> i64 {
        self.prev_mtime
    }

    /// Returns the previous entry's atime.
    #[must_use]
    pub const fn prev_atime(&self) -> i64 {
        self.prev_atime
    }

    /// Returns the previous entry's uid.
    #[must_use]
    pub const fn prev_uid(&self) -> u32 {
        self.prev_uid
    }

    /// Returns the previous entry's gid.
    #[must_use]
    pub const fn prev_gid(&self) -> u32 {
        self.prev_gid
    }

    /// Returns the previous entry's rdev_major.
    #[must_use]
    pub const fn prev_rdev_major(&self) -> u32 {
        self.prev_rdev_major
    }

    /// Returns the previous entry's rdev (protocol < 28).
    #[must_use]
    pub const fn prev_rdev(&self) -> u64 {
        self.prev_rdev
    }

    /// Returns the previous hardlink device number.
    #[must_use]
    pub const fn prev_hardlink_dev(&self) -> i64 {
        self.prev_hardlink_dev
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
        // Reuse existing allocation when possible
        self.prev_name.clear();
        self.prev_name.extend_from_slice(name);
        self.prev_mode = mode;
        self.prev_mtime = mtime;
        self.prev_uid = uid;
        self.prev_gid = gid;
    }

    /// Updates the compression state with all fields from a processed entry.
    ///
    /// This is the comprehensive update method for entries with extended fields.
    #[allow(clippy::too_many_arguments)]
    pub fn update_all(
        &mut self,
        name: &[u8],
        mode: u32,
        mtime: i64,
        atime: i64,
        uid: u32,
        gid: u32,
        rdev_major: u32,
        rdev: u64,
        hardlink_dev: i64,
    ) {
        self.prev_name.clear();
        self.prev_name.extend_from_slice(name);
        self.prev_mode = mode;
        self.prev_mtime = mtime;
        self.prev_atime = atime;
        self.prev_uid = uid;
        self.prev_gid = gid;
        self.prev_rdev_major = rdev_major;
        self.prev_rdev = rdev;
        self.prev_hardlink_dev = hardlink_dev;
    }

    /// Updates only the name portion of the state.
    pub fn update_name(&mut self, name: &[u8]) {
        // Reuse existing allocation when possible
        self.prev_name.clear();
        self.prev_name.extend_from_slice(name);
    }

    /// Updates only the mode portion of the state.
    pub const fn update_mode(&mut self, mode: u32) {
        self.prev_mode = mode;
    }

    /// Updates only the mtime portion of the state.
    pub const fn update_mtime(&mut self, mtime: i64) {
        self.prev_mtime = mtime;
    }

    /// Updates only the uid portion of the state.
    pub const fn update_uid(&mut self, uid: u32) {
        self.prev_uid = uid;
    }

    /// Updates only the gid portion of the state.
    pub const fn update_gid(&mut self, gid: u32) {
        self.prev_gid = gid;
    }

    /// Updates only the rdev_major portion of the state.
    pub const fn update_rdev_major(&mut self, rdev_major: u32) {
        self.prev_rdev_major = rdev_major;
    }

    /// Updates only the rdev portion of the state (protocol < 28).
    pub const fn update_rdev(&mut self, rdev: u64) {
        self.prev_rdev = rdev;
    }

    /// Updates only the atime portion of the state.
    pub const fn update_atime(&mut self, atime: i64) {
        self.prev_atime = atime;
    }

    /// Updates only the hardlink device portion of the state (protocol < 30).
    pub const fn update_hardlink_dev(&mut self, dev: i64) {
        self.prev_hardlink_dev = dev;
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
        assert!(state.prev_name().is_empty());
        assert_eq!(state.prev_mode(), 0);
        assert_eq!(state.prev_mtime(), 0);
        assert_eq!(state.prev_atime(), 0);
        assert_eq!(state.prev_uid(), 0);
        assert_eq!(state.prev_gid(), 0);
        assert_eq!(state.prev_rdev_major(), 0);
        assert_eq!(state.prev_rdev(), 0);
        assert_eq!(state.prev_hardlink_dev(), 0);
    }

    #[test]
    fn calculate_name_prefix_len_empty_prev() {
        let state = FileListCompressionState::new();
        assert_eq!(state.calculate_name_prefix_len(b"test.txt"), 0);
    }

    #[test]
    fn calculate_name_prefix_len_with_prefix() {
        let mut state = FileListCompressionState::new();
        state.update_name(b"dir/file1.txt");

        // "dir/file1.txt" vs "dir/file2.txt" - differs at position 8 ('1' vs '2')
        assert_eq!(state.calculate_name_prefix_len(b"dir/file2.txt"), 8); // "dir/file"
        assert_eq!(state.calculate_name_prefix_len(b"dir/other.txt"), 4); // "dir/"
        assert_eq!(state.calculate_name_prefix_len(b"other/file.txt"), 0);
    }

    #[test]
    fn calculate_name_prefix_len_caps_at_255() {
        let mut state = FileListCompressionState::new();
        state.update_name(&vec![b'a'; 300]);

        let name = vec![b'a'; 300];
        assert_eq!(state.calculate_name_prefix_len(&name), 255);
    }

    #[test]
    fn calculate_name_prefix_len_full_match() {
        let mut state = FileListCompressionState::new();
        state.update_name(b"exact_match");

        assert_eq!(state.calculate_name_prefix_len(b"exact_match"), 11);
    }

    #[test]
    fn update_sets_all_fields() {
        let mut state = FileListCompressionState::new();
        state.update(b"test.txt", 0o644, 1700000000, 1000, 1000);

        assert_eq!(state.prev_name(), b"test.txt");
        assert_eq!(state.prev_mode(), 0o644);
        assert_eq!(state.prev_mtime(), 1700000000);
        assert_eq!(state.prev_uid(), 1000);
        assert_eq!(state.prev_gid(), 1000);
    }

    #[test]
    fn update_individual_fields() {
        let mut state = FileListCompressionState::new();

        state.update_name(b"file.txt");
        assert_eq!(state.prev_name(), b"file.txt");

        state.update_mode(0o755);
        assert_eq!(state.prev_mode(), 0o755);

        state.update_mtime(1234567890);
        assert_eq!(state.prev_mtime(), 1234567890);

        state.update_uid(500);
        assert_eq!(state.prev_uid(), 500);

        state.update_gid(600);
        assert_eq!(state.prev_gid(), 600);

        state.update_rdev_major(8);
        assert_eq!(state.prev_rdev_major(), 8);

        state.update_rdev(0x1234);
        assert_eq!(state.prev_rdev(), 0x1234);
    }

    #[test]
    fn reset_clears_all_fields() {
        let mut state = FileListCompressionState::new();
        state.update(b"test.txt", 0o644, 1700000000, 1000, 1000);
        state.update_hardlink_dev(12345);
        state.update_rdev(0x5678);

        state.reset();

        assert!(state.prev_name().is_empty());
        assert_eq!(state.prev_mode(), 0);
        assert_eq!(state.prev_mtime(), 0);
        assert_eq!(state.prev_atime(), 0);
        assert_eq!(state.prev_uid(), 0);
        assert_eq!(state.prev_gid(), 0);
        assert_eq!(state.prev_rdev_major(), 0);
        assert_eq!(state.prev_rdev(), 0);
        assert_eq!(state.prev_hardlink_dev(), 0);
    }

    #[test]
    fn update_atime() {
        let mut state = FileListCompressionState::new();
        state.update_atime(1700000000);
        assert_eq!(state.prev_atime(), 1700000000);
    }

    #[test]
    fn update_hardlink_dev() {
        let mut state = FileListCompressionState::new();
        state.update_hardlink_dev(0x1234_5678_9ABC);
        assert_eq!(state.prev_hardlink_dev(), 0x1234_5678_9ABC);
    }

    #[test]
    fn update_all_sets_all_fields() {
        let mut state = FileListCompressionState::new();
        state.update_all(
            b"test.txt",
            0o644,
            1700000000,
            1700000001,
            1000,
            1001,
            8,
            0x1234,
            12345,
        );

        assert_eq!(state.prev_name(), b"test.txt");
        assert_eq!(state.prev_mode(), 0o644);
        assert_eq!(state.prev_mtime(), 1700000000);
        assert_eq!(state.prev_atime(), 1700000001);
        assert_eq!(state.prev_uid(), 1000);
        assert_eq!(state.prev_gid(), 1001);
        assert_eq!(state.prev_rdev_major(), 8);
        assert_eq!(state.prev_rdev(), 0x1234);
        assert_eq!(state.prev_hardlink_dev(), 12345);
    }

    #[test]
    fn file_list_stats_default() {
        let stats = FileListStats::default();
        assert_eq!(stats.num_files, 0);
        assert_eq!(stats.num_dirs, 0);
        assert_eq!(stats.num_symlinks, 0);
        assert_eq!(stats.num_devices, 0);
        assert_eq!(stats.num_specials, 0);
        assert_eq!(stats.num_acls, 0);
        assert_eq!(stats.num_xattrs, 0);
        assert_eq!(stats.total_size, 0);
    }

    #[test]
    fn file_list_stats_clone() {
        let stats = FileListStats {
            num_files: 10,
            total_size: 1024,
            ..FileListStats::default()
        };

        let cloned = stats.clone();
        assert_eq!(cloned.num_files, 10);
        assert_eq!(cloned.total_size, 1024);
    }
}
