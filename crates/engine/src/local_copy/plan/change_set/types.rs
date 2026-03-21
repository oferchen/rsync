/// Describes the attributes that changed for a recorded local-copy action.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalCopyChangeSet {
    pub(super) checksum_changed: bool,
    pub(super) size_changed: bool,
    pub(super) time_change: Option<TimeChange>,
    pub(super) permissions_changed: bool,
    pub(super) owner_changed: bool,
    pub(super) group_changed: bool,
    pub(super) access_time_changed: bool,
    pub(super) create_time_changed: bool,
    pub(super) acl_changed: bool,
    pub(super) xattr_changed: bool,
    pub(super) missing_data: bool,
}

/// Describes how the modification time was adjusted for an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeChange {
    /// The modification time now matches the sender's metadata.
    Modified,
    /// The modification time was set to the transfer time because preservation was disabled.
    TransferTime,
}

impl LocalCopyChangeSet {
    /// Returns a change-set with all flags cleared.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            checksum_changed: false,
            size_changed: false,
            time_change: None,
            permissions_changed: false,
            owner_changed: false,
            group_changed: false,
            access_time_changed: false,
            create_time_changed: false,
            acl_changed: false,
            xattr_changed: false,
            missing_data: false,
        }
    }

    /// Marks whether the data checksum changed.
    #[must_use]
    pub const fn with_checksum_changed(mut self, changed: bool) -> Self {
        self.checksum_changed = changed;
        self
    }

    /// Marks whether the logical size changed.
    #[must_use]
    pub const fn with_size_changed(mut self, changed: bool) -> Self {
        self.size_changed = changed;
        self
    }

    /// Records how the modification time changed.
    #[must_use]
    pub const fn with_time_change(mut self, change: Option<TimeChange>) -> Self {
        self.time_change = change;
        self
    }

    /// Marks whether permissions changed.
    #[must_use]
    pub const fn with_permissions_changed(mut self, changed: bool) -> Self {
        self.permissions_changed = changed;
        self
    }

    /// Marks whether ownership changed.
    #[must_use]
    pub const fn with_owner_changed(mut self, changed: bool) -> Self {
        self.owner_changed = changed;
        self
    }

    /// Marks whether the group changed.
    #[must_use]
    pub const fn with_group_changed(mut self, changed: bool) -> Self {
        self.group_changed = changed;
        self
    }

    /// Marks whether the access time changed.
    #[must_use]
    pub const fn with_access_time_changed(mut self, changed: bool) -> Self {
        self.access_time_changed = changed;
        self
    }

    /// Marks whether the create time changed.
    #[must_use]
    pub const fn with_create_time_changed(mut self, changed: bool) -> Self {
        self.create_time_changed = changed;
        self
    }

    /// Marks whether ACLs changed.
    #[must_use]
    pub const fn with_acl_changed(mut self, changed: bool) -> Self {
        self.acl_changed = changed;
        self
    }

    /// Marks whether extended attributes changed.
    #[must_use]
    pub const fn with_xattr_changed(mut self, changed: bool) -> Self {
        self.xattr_changed = changed;
        self
    }

    /// Marks whether the entry has missing data.
    ///
    /// When set, all attribute positions (2-10) in the itemize string display
    /// `?` instead of their normal values.
    ///
    /// # Upstream Reference
    ///
    /// `log.c:730-734` - `ITEM_MISSING_DATA` fills attribute positions with `?`.
    #[must_use]
    pub const fn with_missing_data(mut self, missing: bool) -> Self {
        self.missing_data = missing;
        self
    }

    /// Reports whether the file contents or equivalent metadata changed.
    #[must_use]
    pub const fn checksum_changed(&self) -> bool {
        self.checksum_changed
    }

    /// Reports whether the size changed.
    #[must_use]
    pub const fn size_changed(&self) -> bool {
        self.size_changed
    }

    /// Returns the recorded time change, if any.
    pub const fn time_change(&self) -> Option<TimeChange> {
        self.time_change
    }

    /// Returns the canonical itemize marker for the recorded time change, when any.
    pub const fn time_change_marker(&self) -> Option<char> {
        match self.time_change {
            Some(TimeChange::Modified) => Some('t'),
            Some(TimeChange::TransferTime) => Some('T'),
            None => None,
        }
    }

    /// Reports whether permissions changed.
    #[must_use]
    pub const fn permissions_changed(&self) -> bool {
        self.permissions_changed
    }

    /// Reports whether the owner changed.
    #[must_use]
    pub const fn owner_changed(&self) -> bool {
        self.owner_changed
    }

    /// Reports whether the group changed.
    #[must_use]
    pub const fn group_changed(&self) -> bool {
        self.group_changed
    }

    /// Reports whether the access time changed.
    #[must_use]
    pub const fn access_time_changed(&self) -> bool {
        self.access_time_changed
    }

    /// Reports whether the create time changed.
    #[must_use]
    pub const fn create_time_changed(&self) -> bool {
        self.create_time_changed
    }

    /// Reports whether ACL data changed.
    #[must_use]
    pub const fn acl_changed(&self) -> bool {
        self.acl_changed
    }

    /// Reports whether extended attributes changed.
    #[must_use]
    pub const fn xattr_changed(&self) -> bool {
        self.xattr_changed
    }

    /// Reports whether the entry has missing data.
    ///
    /// When `true`, all attribute positions in the itemize string show `?`.
    #[must_use]
    pub const fn missing_data(&self) -> bool {
        self.missing_data
    }
}
