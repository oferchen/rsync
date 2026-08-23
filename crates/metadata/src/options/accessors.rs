//! Read-only accessor methods for `MetadataOptions`.
//!
//! These methods expose the current configuration state without
//! modification, enabling callers to query which metadata attributes
//! will be preserved during a transfer.

use std::path::Path;

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

    /// Whether a path-based metadata apply may resolve a symlinked parent
    /// instead of being refused by the dirfd-anchored walk in
    /// `fast_io::secure_*_at`.
    ///
    /// Two upstream reasons, either sufficient on its own:
    ///
    /// - `--keep-dirlinks`: the operator opted into following dest-side
    ///   symlinked directories (`generator.c:1356`,
    ///   `link_stat(fname, &sx.st, keep_dirlinks && is_dir)`).
    /// - The entry sits directly under the operator-named destination root and
    ///   that root is a symlink the operator owns. Upstream never meets this
    ///   case at all: it resolves the destination once up front (`main.c:765`
    ///   `change_dir`), so every later syscall sees a real directory -
    ///   `do_lchown` is a bare `lchown(2)` with no sandbox of any kind.
    ///
    /// Only the immediate parent is consulted, because that is the entire
    /// horizon of the walk: it inspects the entry and its parent, never the
    /// grandparent, so an entry deeper than one level never sees the root as a
    /// path component.
    ///
    /// oc is deliberately STRICTER than upstream here. For a non-daemon
    /// receiver upstream's `change_dir` is a plain `chdir` with no ownership
    /// test at all; it walks with `open_no_attacker_symlinks` only when
    /// `am_daemon && !am_chrooted`. oc applies that walk's uid rule - trust
    /// only uid 0 or our euid - on every path, so a destination root raced in
    /// by another uid stays refused where upstream would follow it.
    pub(crate) fn resolves_symlinked_parent(&self, destination: &Path) -> bool {
        self.keep_dirlinks() || self.parent_is_owned_destination_root(destination)
    }

    /// Whether `destination`'s immediate parent is the operator-named
    /// destination root and that root is a symlink the operator owns.
    ///
    /// upstream: `syscall.c:406` - the `ona_open` component walk follows a
    /// symlink only when its `st_uid` is 0 or our euid.
    #[cfg(unix)]
    fn parent_is_owned_destination_root(&self, destination: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;

        let Some(root) = self.destination_root.as_deref() else {
            return false;
        };
        // The operator's destination argument usually keeps its trailing slash
        // (`dst/`), and that slash is load-bearing twice over: `Path::parent`
        // never produces one, and `lstat("sym/")` resolves THROUGH the symlink
        // because a trailing slash demands a directory - so the raw form would
        // both miss the comparison and report the target instead of the link.
        // `components().as_path()` drops it once for both uses.
        let root = root.components().as_path();
        if destination.parent() != Some(root) {
            return false;
        }
        let Ok(metadata) = std::fs::symlink_metadata(root) else {
            return false;
        };
        metadata.file_type().is_symlink() && fast_io::symlink_owner_is_trusted(metadata.uid())
    }

    /// Non-Unix has no uid to consult, so the root is never treated as
    /// operator-owned and every path stays confined.
    #[cfg(not(unix))]
    fn parent_is_owned_destination_root(&self, _destination: &Path) -> bool {
        false
    }
}
