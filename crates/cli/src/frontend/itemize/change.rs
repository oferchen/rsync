//! The `ItemizeChange` struct and its builder methods.
//!
//! Represents an itemized change in the rsync format. Use the builder
//! pattern to construct changes, then call `format()` to generate the
//! 11-character output string.

use super::format::format_itemize;
use super::types::{FileType, UpdateType};

/// Represents an itemized change in the rsync format.
///
/// Use the builder pattern to construct changes, then call `format()` to
/// generate the 11-character output string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemizeChange {
    pub(super) update_type: UpdateType,
    pub(super) file_type: FileType,
    pub(super) new_file: bool,
    pub(super) checksum_changed: bool,
    pub(super) size_changed: bool,
    pub(super) time_changed: bool,
    pub(super) time_set_to_transfer: bool,
    pub(super) perms_changed: bool,
    pub(super) owner_changed: bool,
    pub(super) group_changed: bool,
    pub(super) atime_changed: bool,
    pub(super) ctime_changed: bool,
    pub(super) acl_changed: bool,
    pub(super) xattr_changed: bool,
    pub(super) missing_data: bool,
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
            missing_data: false,
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

    /// Sets whether the entry has missing data.
    ///
    /// When set, all attribute positions (2-10) show `?` instead of their
    /// normal values, mirroring upstream rsync's `ITEM_MISSING_DATA` flag.
    ///
    /// # Upstream Reference
    ///
    /// `log.c:730-734` - `ITEM_MISSING_DATA` fills attribute positions with `?`.
    #[must_use]
    pub const fn with_missing_data(mut self, missing: bool) -> Self {
        self.missing_data = missing;
        self
    }

    /// Returns `true` if all attributes are unchanged (all dots except type indicators).
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        !self.new_file
            && !self.missing_data
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
