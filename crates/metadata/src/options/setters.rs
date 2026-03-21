//! Builder-pattern setter methods for `MetadataOptions`.
//!
//! Each setter consumes and returns `self`, enabling fluent configuration
//! chains. These methods control which metadata attributes are preserved
//! during file transfers.

use crate::chmod::ChmodModifiers;
use crate::{GroupMapping, UserMapping};

use super::MetadataOptions;

impl MetadataOptions {
    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    pub const fn preserve_owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    /// Requests that the group be preserved when applying metadata.
    #[must_use]
    pub const fn preserve_group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Requests that executability be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--executability")]
    #[doc(alias = "-E")]
    pub const fn preserve_executability(mut self, preserve: bool) -> Self {
        self.preserve_executability = preserve;
        self
    }

    /// Requests that permissions be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn preserve_permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Requests that timestamps be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn preserve_times(mut self, preserve: bool) -> Self {
        self.preserve_times = preserve;
        self
    }

    /// Requests that access times be preserved when applying metadata.
    ///
    /// When enabled, the source file's access time (atime) is preserved on the
    /// destination instead of using the current time or the mtime. This corresponds
    /// to the `-U` / `--atimes` flag in upstream rsync.
    ///
    /// Access time preservation only applies to non-directory entries, matching
    /// upstream rsync semantics where directories never have their atime set.
    #[must_use]
    #[doc(alias = "--atimes")]
    #[doc(alias = "-U")]
    pub const fn preserve_atimes(mut self, preserve: bool) -> Self {
        self.preserve_atimes = preserve;
        self
    }

    /// Requests that creation times be preserved when applying metadata.
    ///
    /// When enabled, the source file's creation time (birth time) is preserved
    /// on the destination. This corresponds to the `-N` / `--crtimes` flag in
    /// upstream rsync. Creation time setting is only supported on macOS; on
    /// other platforms the flag is accepted but has no effect.
    #[must_use]
    #[doc(alias = "--crtimes")]
    #[doc(alias = "-N")]
    pub const fn preserve_crtimes(mut self, preserve: bool) -> Self {
        self.preserve_crtimes = preserve;
        self
    }

    /// Requests that UID/GID preservation use numeric identifiers instead of mapping by name.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric: bool) -> Self {
        self.numeric_ids = numeric;
        self
    }

    /// Enables fake-super mode for metadata preservation.
    ///
    /// When enabled, privileged metadata (ownership, device numbers) that cannot
    /// be applied directly is stored in extended attributes (`user.rsync.%stat`)
    /// instead. This allows backup/restore without requiring root privileges.
    #[must_use]
    #[doc(alias = "--fake-super")]
    pub const fn fake_super(mut self, enabled: bool) -> Self {
        self.fake_super = enabled;
        self
    }

    /// Applies an explicit ownership override using numeric identifiers.
    ///
    /// When set, the override takes precedence over [`Self::preserve_owner`]
    /// and [`Self::numeric_ids`] by forcing the supplied UID regardless of the
    /// source metadata. This mirrors the behaviour of rsync's `--chown`
    /// receiver-side handling.
    #[must_use]
    pub const fn with_owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Applies an explicit group override using numeric identifiers.
    ///
    /// When set, the override takes precedence over [`Self::preserve_group`]
    /// and [`Self::numeric_ids`] by forcing the supplied GID regardless of the
    /// source metadata. This mirrors the behaviour of rsync's `--chown`
    /// receiver-side handling.
    #[must_use]
    pub const fn with_group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Supplies chmod modifiers that should be applied after metadata is
    /// preserved.
    #[must_use]
    pub fn with_chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Applies a custom user mapping derived from `--usermap`.
    #[must_use]
    pub fn with_user_mapping(mut self, mapping: Option<UserMapping>) -> Self {
        self.user_mapping = mapping;
        self
    }

    /// Applies a custom group mapping derived from `--groupmap`.
    #[must_use]
    pub fn with_group_mapping(mut self, mapping: Option<GroupMapping>) -> Self {
        self.group_mapping = mapping;
        self
    }
}
