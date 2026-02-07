#![deny(unsafe_code)]

//! Implements upstream rsync's --itemize-changes (-i) output format.
//!
//! The itemize format is an 11-character string: `YXcstpoguax`
//!
//! - Position 0 (Y): Update type
//!   - `<` sent to remote
//!   - `>` received from remote
//!   - `c` local change (created)
//!   - `h` hard link
//!   - `.` not updated
//!   - `*` message (e.g., `*deleting`)
//! - Position 1 (X): File type
//!   - `f` regular file
//!   - `d` directory
//!   - `L` symlink
//!   - `D` device (char or block)
//!   - `S` special file (fifo, socket)
//! - Positions 2-10: Attribute changes
//!   - Position 2 (c): checksum differs (or `+` for new file)
//!   - Position 3 (s): size differs
//!   - Position 4 (t): modification time differs (`t`) or set to transfer time (`T`)
//!   - Position 5 (p): permissions differ
//!   - Position 6 (o): owner differs
//!   - Position 7 (g): group differs
//!   - Position 8 (u): reserved for atime/ctime (`u` = atime, `n` = ctime, `b` = both)
//!   - Position 9 (a): ACL differs
//!   - Position 10 (x): extended attributes differ
//!
//! All unchanged attributes show `.` (dot). New files show `+` for all attributes.
//!
//! # Examples
//!
//! ```
//! use cli::ItemizeChange;
//!
//! // New file
//! let change = ItemizeChange::new()
//!     .with_update_type(cli::UpdateType::Received)
//!     .with_file_type(cli::FileType::RegularFile)
//!     .with_new_file(true);
//! assert_eq!(change.format(), ">f+++++++++");
//!
//! // File with checksum and size changed
//! let change = ItemizeChange::new()
//!     .with_update_type(cli::UpdateType::Received)
//!     .with_file_type(cli::FileType::RegularFile)
//!     .with_checksum_changed(true)
//!     .with_size_changed(true);
//! assert_eq!(change.format(), ">fcs.......");
//!
//! // Unchanged file
//! let change = ItemizeChange::new()
//!     .with_update_type(cli::UpdateType::NotUpdated)
//!     .with_file_type(cli::FileType::RegularFile);
//! assert_eq!(change.format(), ".f.........");
//! ```

/// Update type indicator (position 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateType {
    /// `<` - sent to remote
    Sent,
    /// `>` - received from remote
    Received,
    /// `c` - local change (created)
    Created,
    /// `h` - hard link
    HardLink,
    /// `.` - not updated
    NotUpdated,
    /// `*` - message follows (e.g., `*deleting`)
    Message,
}

impl UpdateType {
    /// Returns the character representation for position 0.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Sent => '<',
            Self::Received => '>',
            Self::Created => 'c',
            Self::HardLink => 'h',
            Self::NotUpdated => '.',
            Self::Message => '*',
        }
    }
}

/// File type indicator (position 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// `f` - regular file
    RegularFile,
    /// `d` - directory
    Directory,
    /// `L` - symlink
    Symlink,
    /// `D` - device (char or block)
    Device,
    /// `S` - special file (fifo, socket)
    Special,
}

impl FileType {
    /// Returns the character representation for position 1.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::RegularFile => 'f',
            Self::Directory => 'd',
            Self::Symlink => 'L',
            Self::Device => 'D',
            Self::Special => 'S',
        }
    }
}

/// Represents an itemized change in the rsync format.
///
/// Use the builder pattern to construct changes, then call `format()` to
/// generate the 11-character output string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemizeChange {
    update_type: UpdateType,
    file_type: FileType,
    new_file: bool,
    checksum_changed: bool,
    size_changed: bool,
    time_changed: bool,
    time_set_to_transfer: bool,
    perms_changed: bool,
    owner_changed: bool,
    group_changed: bool,
    atime_changed: bool,
    ctime_changed: bool,
    acl_changed: bool,
    xattr_changed: bool,
}

impl ItemizeChange {
    /// Creates a new itemized change with all attributes unchanged.
    ///
    /// Defaults to `NotUpdated` update type and `RegularFile` file type.
    /// All attribute flags are set to `false`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            update_type: UpdateType::NotUpdated,
            file_type: FileType::RegularFile,
            new_file: false,
            checksum_changed: false,
            size_changed: false,
            time_changed: false,
            time_set_to_transfer: false,
            perms_changed: false,
            owner_changed: false,
            group_changed: false,
            atime_changed: false,
            ctime_changed: false,
            acl_changed: false,
            xattr_changed: false,
        }
    }

    /// Sets the update type (position 0).
    #[must_use]
    pub const fn with_update_type(mut self, update_type: UpdateType) -> Self {
        self.update_type = update_type;
        self
    }

    /// Sets the file type (position 1).
    #[must_use]
    pub const fn with_file_type(mut self, file_type: FileType) -> Self {
        self.file_type = file_type;
        self
    }

    /// Marks this as a new file (all attributes show `+`).
    #[must_use]
    pub const fn with_new_file(mut self, new_file: bool) -> Self {
        self.new_file = new_file;
        self
    }

    /// Sets whether the checksum changed (position 2).
    #[must_use]
    pub const fn with_checksum_changed(mut self, changed: bool) -> Self {
        self.checksum_changed = changed;
        self
    }

    /// Sets whether the size changed (position 3).
    #[must_use]
    pub const fn with_size_changed(mut self, changed: bool) -> Self {
        self.size_changed = changed;
        self
    }

    /// Sets whether the modification time changed (position 4).
    ///
    /// Shows lowercase `t` when this is set.
    #[must_use]
    pub const fn with_time_changed(mut self, changed: bool) -> Self {
        self.time_changed = changed;
        self
    }

    /// Sets whether the time was set to transfer time (position 4).
    ///
    /// Shows uppercase `T` when this is set (takes precedence over `time_changed`).
    #[must_use]
    pub const fn with_time_set_to_transfer(mut self, set: bool) -> Self {
        self.time_set_to_transfer = set;
        self
    }

    /// Sets whether the permissions changed (position 5).
    #[must_use]
    pub const fn with_perms_changed(mut self, changed: bool) -> Self {
        self.perms_changed = changed;
        self
    }

    /// Sets whether the owner changed (position 6).
    #[must_use]
    pub const fn with_owner_changed(mut self, changed: bool) -> Self {
        self.owner_changed = changed;
        self
    }

    /// Sets whether the group changed (position 7).
    #[must_use]
    pub const fn with_group_changed(mut self, changed: bool) -> Self {
        self.group_changed = changed;
        self
    }

    /// Sets whether the access time changed (position 8).
    ///
    /// Shows `u` when only atime changed, `b` when both atime and ctime changed.
    #[must_use]
    pub const fn with_atime_changed(mut self, changed: bool) -> Self {
        self.atime_changed = changed;
        self
    }

    /// Sets whether the creation time changed (position 8).
    ///
    /// Shows `n` when only ctime changed, `b` when both atime and ctime changed.
    #[must_use]
    pub const fn with_ctime_changed(mut self, changed: bool) -> Self {
        self.ctime_changed = changed;
        self
    }

    /// Sets whether the ACL changed (position 9).
    #[must_use]
    pub const fn with_acl_changed(mut self, changed: bool) -> Self {
        self.acl_changed = changed;
        self
    }

    /// Sets whether the extended attributes changed (position 10).
    #[must_use]
    pub const fn with_xattr_changed(mut self, changed: bool) -> Self {
        self.xattr_changed = changed;
        self
    }

    /// Returns `true` if all attributes are unchanged (all dots except type indicators).
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        !self.new_file
            && !self.checksum_changed
            && !self.size_changed
            && !self.time_changed
            && !self.time_set_to_transfer
            && !self.perms_changed
            && !self.owner_changed
            && !self.group_changed
            && !self.atime_changed
            && !self.ctime_changed
            && !self.acl_changed
            && !self.xattr_changed
    }

    /// Formats this change as an 11-character rsync itemize string.
    ///
    /// # Examples
    ///
    /// ```
    /// use cli::{ItemizeChange, UpdateType, FileType};
    ///
    /// let change = ItemizeChange::new()
    ///     .with_update_type(UpdateType::Received)
    ///     .with_file_type(FileType::RegularFile)
    ///     .with_checksum_changed(true)
    ///     .with_size_changed(true)
    ///     .with_time_changed(true);
    ///
    /// assert_eq!(change.format(), ">fcst......");
    /// ```
    #[must_use]
    pub fn format(&self) -> String {
        format_itemize(self)
    }
}

impl Default for ItemizeChange {
    fn default() -> Self {
        Self::new()
    }
}

/// Formats an itemized change as an 11-character rsync string.
///
/// This is the core formatting function that produces strings like:
/// - `>f+++++++++` - new file received
/// - `.f.........` - unchanged file
/// - `>fcst......` - file with checksum, size, and time changes
///
/// # Examples
///
/// ```
/// use cli::{ItemizeChange, UpdateType, FileType, format_itemize};
///
/// let change = ItemizeChange::new()
///     .with_update_type(UpdateType::Received)
///     .with_file_type(FileType::Directory)
///     .with_new_file(true);
///
/// assert_eq!(format_itemize(&change), ">d+++++++++");
/// ```
#[must_use]
pub fn format_itemize(change: &ItemizeChange) -> String {
    let mut result = String::with_capacity(11);

    // Position 0: Update type
    result.push(change.update_type.as_char());

    // Position 1: File type
    result.push(change.file_type.as_char());

    // Positions 2-10: Attributes
    if change.new_file {
        // New files show '+' for all attribute positions
        result.push_str("+++++++++");
    } else {
        // Position 2: Checksum
        result.push(if change.checksum_changed { 'c' } else { '.' });

        // Position 3: Size
        result.push(if change.size_changed { 's' } else { '.' });

        // Position 4: Time (T takes precedence over t)
        result.push(if change.time_set_to_transfer {
            'T'
        } else if change.time_changed {
            't'
        } else {
            '.'
        });

        // Position 5: Permissions
        result.push(if change.perms_changed { 'p' } else { '.' });

        // Position 6: Owner
        result.push(if change.owner_changed { 'o' } else { '.' });

        // Position 7: Group
        result.push(if change.group_changed { 'g' } else { '.' });

        // Position 8: Access/Create time (both, atime only, ctime only, or neither)
        result.push(match (change.atime_changed, change.ctime_changed) {
            (true, true) => 'b',
            (true, false) => 'u',
            (false, true) => 'n',
            (false, false) => '.',
        });

        // Position 9: ACL
        result.push(if change.acl_changed { 'a' } else { '.' });

        // Position 10: Extended attributes
        result.push(if change.xattr_changed { 'x' } else { '.' });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Update type character conversion ----

    #[test]
    fn update_type_sent_is_less_than() {
        assert_eq!(UpdateType::Sent.as_char(), '<');
    }

    #[test]
    fn update_type_received_is_greater_than() {
        assert_eq!(UpdateType::Received.as_char(), '>');
    }

    #[test]
    fn update_type_created_is_c() {
        assert_eq!(UpdateType::Created.as_char(), 'c');
    }

    #[test]
    fn update_type_hard_link_is_h() {
        assert_eq!(UpdateType::HardLink.as_char(), 'h');
    }

    #[test]
    fn update_type_not_updated_is_dot() {
        assert_eq!(UpdateType::NotUpdated.as_char(), '.');
    }

    #[test]
    fn update_type_message_is_star() {
        assert_eq!(UpdateType::Message.as_char(), '*');
    }

    // ---- File type character conversion ----

    #[test]
    fn file_type_regular_file_is_f() {
        assert_eq!(FileType::RegularFile.as_char(), 'f');
    }

    #[test]
    fn file_type_directory_is_d() {
        assert_eq!(FileType::Directory.as_char(), 'd');
    }

    #[test]
    fn file_type_symlink_is_l_upper() {
        assert_eq!(FileType::Symlink.as_char(), 'L');
    }

    #[test]
    fn file_type_device_is_d_upper() {
        assert_eq!(FileType::Device.as_char(), 'D');
    }

    #[test]
    fn file_type_special_is_s_upper() {
        assert_eq!(FileType::Special.as_char(), 'S');
    }

    // ---- ItemizeChange construction and defaults ----

    #[test]
    fn new_creates_unchanged_regular_file() {
        let change = ItemizeChange::new();
        assert_eq!(change.update_type, UpdateType::NotUpdated);
        assert_eq!(change.file_type, FileType::RegularFile);
        assert!(!change.new_file);
        assert!(change.is_unchanged());
    }

    #[test]
    fn default_is_same_as_new() {
        assert_eq!(ItemizeChange::default(), ItemizeChange::new());
    }

    // ---- Basic formatting: unchanged file ----

    #[test]
    fn format_unchanged_file() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::NotUpdated)
            .with_file_type(FileType::RegularFile);
        assert_eq!(change.format(), ".f.........");
    }

    // ---- Basic formatting: new file ----

    #[test]
    fn format_new_file() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_new_file(true);
        assert_eq!(change.format(), ">f+++++++++");
    }

    // ---- Basic formatting: updated file with checksum+size ----

    #[test]
    fn format_updated_file_checksum_and_size() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_checksum_changed(true)
            .with_size_changed(true);
        assert_eq!(change.format(), ">fcs.......");
    }

    // ---- Basic formatting: sent file ----

    #[test]
    fn format_sent_file() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Sent)
            .with_file_type(FileType::RegularFile);
        assert_eq!(change.format(), "<f.........");
    }

    // ---- Basic formatting: directory ----

    #[test]
    fn format_directory() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::NotUpdated)
            .with_file_type(FileType::Directory);
        assert_eq!(change.format(), ".d.........");
    }

    // ---- Basic formatting: symlink ----

    #[test]
    fn format_symlink() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::NotUpdated)
            .with_file_type(FileType::Symlink);
        assert_eq!(change.format(), ".L.........");
    }

    // ---- All flags changed ----

    #[test]
    fn format_all_flags_changed() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_changed(true)
            .with_perms_changed(true)
            .with_owner_changed(true)
            .with_group_changed(true)
            .with_atime_changed(true)
            .with_ctime_changed(true)
            .with_acl_changed(true)
            .with_xattr_changed(true);
        assert_eq!(change.format(), ">fcstpogbax");
    }

    // ---- Time changed ----

    #[test]
    fn format_time_changed() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_time_changed(true);
        assert_eq!(change.format(), ">f..t......");
    }

    // ---- Permissions changed ----

    #[test]
    fn format_perms_changed() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_perms_changed(true);
        assert_eq!(change.format(), ">f...p.....");
    }

    // ---- Owner and group changed ----

    #[test]
    fn format_owner_and_group_changed() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_owner_changed(true)
            .with_group_changed(true);
        assert_eq!(change.format(), ">f....og...");
    }

    // ---- ACL and xattr changed ----

    #[test]
    fn format_acl_and_xattr_changed() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_acl_changed(true)
            .with_xattr_changed(true);
        assert_eq!(change.format(), ">f.......ax");
    }

    // ---- Created file with all new indicators ----

    #[test]
    fn format_created_file_all_new() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Created)
            .with_file_type(FileType::RegularFile)
            .with_new_file(true);
        assert_eq!(change.format(), "cf+++++++++");
    }

    // ---- Builder pattern creates correct output ----

    #[test]
    fn builder_pattern_works() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::Directory)
            .with_time_changed(true);
        assert_eq!(change.format(), ">d..t......");
    }

    // ---- is_unchanged detection ----

    #[test]
    fn is_unchanged_returns_true_for_no_changes() {
        let change = ItemizeChange::new();
        assert!(change.is_unchanged());
    }

    #[test]
    fn is_unchanged_returns_false_for_new_file() {
        let change = ItemizeChange::new().with_new_file(true);
        assert!(!change.is_unchanged());
    }

    #[test]
    fn is_unchanged_returns_false_when_checksum_changed() {
        let change = ItemizeChange::new().with_checksum_changed(true);
        assert!(!change.is_unchanged());
    }

    #[test]
    fn is_unchanged_returns_false_when_any_flag_set() {
        let change = ItemizeChange::new().with_acl_changed(true);
        assert!(!change.is_unchanged());
    }

    // ---- Edge case: device file ----

    #[test]
    fn format_device_file() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Created)
            .with_file_type(FileType::Device)
            .with_new_file(true);
        assert_eq!(change.format(), "cD+++++++++");
    }

    // ---- Edge case: special file ----

    #[test]
    fn format_special_file() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Created)
            .with_file_type(FileType::Special)
            .with_new_file(true);
        assert_eq!(change.format(), "cS+++++++++");
    }

    // ---- Time set to transfer (uppercase T) ----

    #[test]
    fn format_time_set_to_transfer() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_time_set_to_transfer(true);
        assert_eq!(change.format(), ">f..T......");
    }

    #[test]
    fn time_set_to_transfer_overrides_time_changed() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_time_changed(true)
            .with_time_set_to_transfer(true);
        assert_eq!(change.format(), ">f..T......");
    }

    // ---- Access time only (u) ----

    #[test]
    fn format_atime_changed_only() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_atime_changed(true);
        assert_eq!(change.format(), ">f......u..");
    }

    // ---- Create time only (n) ----

    #[test]
    fn format_ctime_changed_only() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_ctime_changed(true);
        assert_eq!(change.format(), ">f......n..");
    }

    // ---- Both access and create time (b) ----

    #[test]
    fn format_both_atime_and_ctime_changed() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_atime_changed(true)
            .with_ctime_changed(true);
        assert_eq!(change.format(), ">f......b..");
    }

    // ---- Hard link ----

    #[test]
    fn format_hard_link() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::HardLink)
            .with_file_type(FileType::RegularFile)
            .with_new_file(true);
        assert_eq!(change.format(), "hf+++++++++");
    }

    // ---- Message type ----

    #[test]
    fn format_message_type() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Message)
            .with_file_type(FileType::RegularFile);
        assert_eq!(change.format(), "*f.........");
    }

    // ---- Standalone format_itemize function ----

    #[test]
    fn format_itemize_standalone_works() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_changed(true);
        assert_eq!(format_itemize(&change), ">fcst......");
    }

    // ---- Length is always 11 ----

    #[test]
    fn format_length_is_always_eleven() {
        let changes = vec![
            ItemizeChange::new(),
            ItemizeChange::new().with_new_file(true),
            ItemizeChange::new()
                .with_update_type(UpdateType::Received)
                .with_file_type(FileType::Directory)
                .with_checksum_changed(true)
                .with_size_changed(true)
                .with_time_changed(true)
                .with_perms_changed(true)
                .with_owner_changed(true)
                .with_group_changed(true)
                .with_atime_changed(true)
                .with_ctime_changed(true)
                .with_acl_changed(true)
                .with_xattr_changed(true),
        ];

        for change in changes {
            let formatted = change.format();
            assert_eq!(
                formatted.len(),
                11,
                "format should be 11 chars: {formatted:?}"
            );
        }
    }

    // ---- New file ignores individual change flags ----

    #[test]
    fn new_file_overrides_individual_flags() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_new_file(true)
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_changed(true);
        assert_eq!(change.format(), ">f+++++++++");
    }

    // ---- Typical content update pattern ----

    #[test]
    fn format_typical_content_update() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::RegularFile)
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_changed(true);
        assert_eq!(change.format(), ">fcst......");
    }

    // ---- Directory with time update ----

    #[test]
    fn format_directory_time_update() {
        let change = ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::Directory)
            .with_time_changed(true);
        assert_eq!(change.format(), ">d..t......");
    }
}
