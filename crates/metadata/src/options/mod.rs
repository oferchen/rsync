use crate::chmod::ChmodModifiers;
use crate::{GroupMapping, UserMapping};

/// Bitflags controlling which time-related attributes to skip or force when
/// applying metadata.
///
/// These flags mirror upstream rsync's `ATTRS_*` constants from `rsync.h:192-196`
/// and are passed to `set_file_attrs()` to govern which timestamps are applied
/// and under what tolerance. The default value (`empty()`) means "apply all
/// requested time attributes normally using `modify_window` tolerance."
///
/// # Upstream Reference
///
/// - `rsync.h:192-196` - `ATTRS_REPORT`, `ATTRS_SKIP_MTIME`, `ATTRS_ACCURATE_TIME`,
///   `ATTRS_SKIP_ATIME`, `ATTRS_SKIP_CRTIME`
/// - `rsync.c:585-597` - Used in `set_file_attrs()` to conditionally apply mtime,
///   atime, and crtime
/// - `generator.c:1814` - `maybe_ATTRS_REPORT | maybe_ATTRS_ACCURATE_TIME` on quick-check
///   match paths
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct AttrsFlags(u8);

impl AttrsFlags {
    /// Report attribute changes to the log. When set, `set_file_attrs` emits
    /// verbose messages for unchanged files.
    ///
    /// Upstream: `rsync.h:192` — `ATTRS_REPORT (1<<0)`
    pub const REPORT: Self = Self(1 << 0);

    /// Skip modification time comparison and application entirely. When set,
    /// `set_file_attrs` will not touch the destination's mtime regardless of
    /// whether `preserve_times` is enabled.
    ///
    /// Upstream: `rsync.h:193` — `ATTRS_SKIP_MTIME (1<<1)`
    pub const SKIP_MTIME: Self = Self(1 << 1);

    /// Force exact time comparison instead of using `modify_window` tolerance.
    /// Used after a transfer or checksum verification where the timestamp was
    /// just set and should match precisely.
    ///
    /// Upstream: `rsync.h:194` — `ATTRS_ACCURATE_TIME (1<<2)`
    pub const ACCURATE_TIME: Self = Self(1 << 2);

    /// Skip access time comparison and application entirely. When set,
    /// `set_file_attrs` will not touch the destination's atime regardless of
    /// whether `preserve_atimes` is enabled.
    ///
    /// Upstream: `rsync.h:195` — `ATTRS_SKIP_ATIME (1<<3)`
    pub const SKIP_ATIME: Self = Self(1 << 3);

    /// Skip creation time comparison and application entirely. When set,
    /// `set_file_attrs` will not touch the destination's crtime regardless of
    /// whether `preserve_crtimes` is enabled.
    ///
    /// Upstream: `rsync.h:196` — `ATTRS_SKIP_CRTIME (1<<5)`
    ///
    /// Note: bit 4 is unused in upstream, matching the gap between
    /// `ATTRS_SKIP_ATIME (1<<3)` and `ATTRS_SKIP_CRTIME (1<<5)`.
    pub const SKIP_CRTIME: Self = Self(1 << 5);

    /// Convenience constant combining all three time-skip flags.
    ///
    /// Used when directory times or link times should be omitted entirely.
    ///
    /// Upstream: `rsync.c:585` — `flags |= ATTRS_SKIP_MTIME | ATTRS_SKIP_ATIME | ATTRS_SKIP_CRTIME`
    pub const SKIP_ALL_TIMES: Self =
        Self(Self::SKIP_MTIME.0 | Self::SKIP_ATIME.0 | Self::SKIP_CRTIME.0);

    /// Returns an empty flags value (no attributes skipped).
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Returns `true` when no flags are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the raw `u8` representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Creates flags from a raw `u8` value.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns `true` when the given flag is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `true` when `ATTRS_SKIP_MTIME` is set.
    #[must_use]
    pub const fn skip_mtime(self) -> bool {
        self.0 & Self::SKIP_MTIME.0 != 0
    }

    /// Returns `true` when `ATTRS_SKIP_ATIME` is set.
    #[must_use]
    pub const fn skip_atime(self) -> bool {
        self.0 & Self::SKIP_ATIME.0 != 0
    }

    /// Returns `true` when `ATTRS_SKIP_CRTIME` is set.
    #[must_use]
    pub const fn skip_crtime(self) -> bool {
        self.0 & Self::SKIP_CRTIME.0 != 0
    }

    /// Returns `true` when `ATTRS_ACCURATE_TIME` is set.
    #[must_use]
    pub const fn accurate_time(self) -> bool {
        self.0 & Self::ACCURATE_TIME.0 != 0
    }

    /// Returns `true` when `ATTRS_REPORT` is set.
    #[must_use]
    pub const fn report(self) -> bool {
        self.0 & Self::REPORT.0 != 0
    }
}

impl std::ops::BitOr for AttrsFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for AttrsFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for AttrsFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Options that control metadata preservation during copy operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataOptions {
    preserve_owner: bool,
    preserve_group: bool,
    preserve_executability: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    preserve_atimes: bool,
    preserve_crtimes: bool,
    numeric_ids: bool,
    fake_super: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    chmod: Option<ChmodModifiers>,
    user_mapping: Option<UserMapping>,
    group_mapping: Option<GroupMapping>,
}

impl MetadataOptions {
    /// Creates a new [`MetadataOptions`] value with defaults applied.
    ///
    /// By default the options preserve permissions and timestamps while leaving
    /// ownership disabled so callers can opt-in as needed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            preserve_owner: false,
            preserve_group: false,
            preserve_executability: false,
            preserve_permissions: true,
            preserve_times: true,
            preserve_atimes: false,
            preserve_crtimes: false,
            numeric_ids: false,
            fake_super: false,
            owner_override: None,
            group_override: None,
            chmod: None,
            user_mapping: None,
            group_mapping: None,
        }
    }

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

impl Default for MetadataOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
