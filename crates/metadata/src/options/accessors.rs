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

    /// Reports whether the destination file was newly created during this transfer.
    ///
    /// upstream: rsync.c:dest_mode() distinguishes new vs existing files.
    #[must_use]
    pub const fn destination_is_new(&self) -> bool {
        self.destination_is_new
    }

    /// Reports whether `--keep-dirlinks` is active.
    ///
    /// When this returns `true`, callers that would otherwise refuse to walk
    /// through symlinked parents (e.g. the dirfd-anchored TOCTOU sandbox in
    /// `fast_io::secure_chmod_at`) must bypass that guard, because the user
    /// has explicitly opted into following dest-side symlinks-to-dirs.
    ///
    /// upstream: generator.c:1356 - `link_stat(fname, &sx.st, keep_dirlinks && is_dir)`.
    #[must_use]
    pub const fn keep_dirlinks(&self) -> bool {
        self.keep_dirlinks
    }

    /// Returns `true` when at least one metadata preservation flag is active.
    ///
    /// When this returns `false`, `apply_metadata_with_cached_stat` is a no-op
    /// because none of the ownership, permission, or timestamp sub-functions
    /// will issue any syscalls. Callers can skip the entire metadata application
    /// chain on the no-change path.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:set_file_attrs()` - only applies ownership, permissions, and
    ///   timestamps when the corresponding global flags are set
    #[must_use]
    pub const fn has_any_preservation(&self) -> bool {
        self.preserve_owner
            || self.preserve_group
            || self.preserve_executability
            || self.preserve_permissions
            || self.preserve_times
            || self.preserve_atimes
            || self.preserve_crtimes
            || self.fake_super
            || self.owner_override.is_some()
            || self.group_override.is_some()
            || self.chmod.is_some()
    }

    /// Returns `true` when at least one metadata preservation flag is active.
    ///
    /// Used by the receiver's quick-check skip path to avoid entering the
    /// `apply_metadata_with_cached_stat` call chain when no attributes would
    /// be inspected. Each inner function (`apply_ownership_from_entry`,
    /// `apply_permissions_from_entry`, `apply_timestamps_from_entry`) already
    /// has its own early-exit guard, but skipping the entire chain saves the
    /// function-call overhead per file.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:574-625` - `set_file_attrs()` is unconditionally called for
    ///   quick-check matched files. oc-rsync mirrors this by always calling the
    ///   apply chain when `requires_apply()` returns true.
    #[must_use]
    pub const fn requires_apply(&self) -> bool {
        self.preserve_owner
            || self.preserve_group
            || self.preserve_executability
            || self.preserve_permissions
            || self.preserve_times
            || self.preserve_atimes
            || self.preserve_crtimes
            || self.fake_super
            || self.owner_override.is_some()
            || self.group_override.is_some()
            || self.chmod.is_some()
    }
}
