//! Read-only accessor methods for `MetadataOptions`.
//!
//! These methods expose the current configuration state without
//! modification, enabling callers to query which metadata attributes
//! will be preserved during a transfer.

use crate::chmod::ChmodModifiers;
use crate::{GroupMapping, UserMapping};

use super::MetadataOptions;

impl MetadataOptions {
    /// Reports whether ownership should be preserved.
    #[must_use]
    pub const fn owner(&self) -> bool {
        self.preserve_owner
    }

    /// Reports whether the group should be preserved.
    #[must_use]
    pub const fn group(&self) -> bool {
        self.preserve_group
    }

    /// Reports whether executability should be preserved.
    #[must_use]
    pub const fn executability(&self) -> bool {
        self.preserve_executability
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    pub const fn permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    pub const fn times(&self) -> bool {
        self.preserve_times
    }

    /// Reports whether access times should be preserved.
    #[must_use]
    pub const fn atimes(&self) -> bool {
        self.preserve_atimes
    }

    /// Reports whether creation times should be preserved.
    #[must_use]
    pub const fn crtimes(&self) -> bool {
        self.preserve_crtimes
    }

    /// Reports whether numeric UID/GID preservation was requested.
    #[must_use]
    pub const fn numeric_ids_enabled(&self) -> bool {
        self.numeric_ids
    }

    /// Reports whether fake-super mode is enabled.
    #[must_use]
    pub const fn fake_super_enabled(&self) -> bool {
        self.fake_super
    }

    /// Reports the configured ownership override if any.
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports the configured group override if any.
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Returns the chmod modifiers, if any.
    pub const fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Returns the configured user mapping, if any.
    pub const fn user_mapping(&self) -> Option<&UserMapping> {
        self.user_mapping.as_ref()
    }

    /// Returns the configured group mapping, if any.
    pub const fn group_mapping(&self) -> Option<&GroupMapping> {
        self.group_mapping.as_ref()
    }
}
