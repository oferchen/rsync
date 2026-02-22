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
    #[must_use]
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports the configured group override if any.
    #[must_use]
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Returns the chmod modifiers, if any.
    #[must_use]
    pub const fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Returns the configured user mapping, if any.
    #[must_use]
    pub const fn user_mapping(&self) -> Option<&UserMapping> {
        self.user_mapping.as_ref()
    }

    /// Returns the configured group mapping, if any.
    #[must_use]
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
mod tests {
    use super::*;
    use crate::chmod::ChmodModifiers;

    // ==================== AttrsFlags tests ====================

    #[test]
    fn attrs_flags_constants_match_upstream() {
        // upstream rsync.h:192-196
        assert_eq!(AttrsFlags::REPORT.bits(), 1 << 0);
        assert_eq!(AttrsFlags::SKIP_MTIME.bits(), 1 << 1);
        assert_eq!(AttrsFlags::ACCURATE_TIME.bits(), 1 << 2);
        assert_eq!(AttrsFlags::SKIP_ATIME.bits(), 1 << 3);
        assert_eq!(AttrsFlags::SKIP_CRTIME.bits(), 1 << 5);
    }

    #[test]
    fn attrs_flags_skip_all_times_combines_three_skip_flags() {
        let combined = AttrsFlags::SKIP_MTIME | AttrsFlags::SKIP_ATIME | AttrsFlags::SKIP_CRTIME;
        assert_eq!(AttrsFlags::SKIP_ALL_TIMES, combined);
        assert!(AttrsFlags::SKIP_ALL_TIMES.skip_mtime());
        assert!(AttrsFlags::SKIP_ALL_TIMES.skip_atime());
        assert!(AttrsFlags::SKIP_ALL_TIMES.skip_crtime());
        assert!(!AttrsFlags::SKIP_ALL_TIMES.accurate_time());
        assert!(!AttrsFlags::SKIP_ALL_TIMES.report());
    }

    #[test]
    fn attrs_flags_empty_has_no_bits_set() {
        let flags = AttrsFlags::empty();
        assert!(flags.is_empty());
        assert_eq!(flags.bits(), 0);
        assert!(!flags.skip_mtime());
        assert!(!flags.skip_atime());
        assert!(!flags.skip_crtime());
        assert!(!flags.accurate_time());
        assert!(!flags.report());
    }

    #[test]
    fn attrs_flags_default_is_empty() {
        assert_eq!(AttrsFlags::default(), AttrsFlags::empty());
    }

    #[test]
    fn attrs_flags_from_bits_round_trips() {
        let flags = AttrsFlags::SKIP_MTIME | AttrsFlags::ACCURATE_TIME;
        let raw = flags.bits();
        assert_eq!(AttrsFlags::from_bits(raw), flags);
    }

    #[test]
    fn attrs_flags_contains_checks_subset() {
        let all = AttrsFlags::SKIP_ALL_TIMES;
        assert!(all.contains(AttrsFlags::SKIP_MTIME));
        assert!(all.contains(AttrsFlags::SKIP_ATIME));
        assert!(all.contains(AttrsFlags::SKIP_CRTIME));
        assert!(!all.contains(AttrsFlags::REPORT));
        assert!(!all.contains(AttrsFlags::ACCURATE_TIME));
    }

    #[test]
    fn attrs_flags_union_combines_flags() {
        let a = AttrsFlags::REPORT;
        let b = AttrsFlags::SKIP_MTIME;
        let combined = a.union(b);
        assert!(combined.report());
        assert!(combined.skip_mtime());
        assert!(!combined.skip_atime());
    }

    #[test]
    fn attrs_flags_bitor_assign() {
        let mut flags = AttrsFlags::empty();
        flags |= AttrsFlags::SKIP_MTIME;
        flags |= AttrsFlags::SKIP_ATIME;
        assert!(flags.skip_mtime());
        assert!(flags.skip_atime());
        assert!(!flags.skip_crtime());
    }

    #[test]
    fn attrs_flags_bitand_masks() {
        let flags = AttrsFlags::SKIP_ALL_TIMES | AttrsFlags::REPORT;
        let masked = flags & AttrsFlags::SKIP_MTIME;
        assert!(masked.skip_mtime());
        assert!(!masked.skip_atime());
        assert!(!masked.report());
    }

    #[test]
    fn attrs_flags_bit4_is_unused_gap() {
        // upstream rsync.h has a gap between bit 3 (SKIP_ATIME) and bit 5
        // (SKIP_CRTIME). Bit 4 is unused, matching upstream layout.
        assert_eq!(AttrsFlags::SKIP_ATIME.bits(), 0x08);
        assert_eq!(AttrsFlags::SKIP_CRTIME.bits(), 0x20);
        // No constant occupies bit 4 (0x10)
    }

    #[test]
    fn attrs_flags_individual_predicate_methods() {
        assert!(AttrsFlags::REPORT.report());
        assert!(!AttrsFlags::REPORT.skip_mtime());

        assert!(AttrsFlags::SKIP_MTIME.skip_mtime());
        assert!(!AttrsFlags::SKIP_MTIME.skip_atime());

        assert!(AttrsFlags::ACCURATE_TIME.accurate_time());
        assert!(!AttrsFlags::ACCURATE_TIME.skip_mtime());

        assert!(AttrsFlags::SKIP_ATIME.skip_atime());
        assert!(!AttrsFlags::SKIP_ATIME.skip_crtime());

        assert!(AttrsFlags::SKIP_CRTIME.skip_crtime());
        assert!(!AttrsFlags::SKIP_CRTIME.skip_mtime());
    }

    #[test]
    fn attrs_flags_upstream_set_file_attrs_scenario() {
        // Mirrors upstream rsync.c:583-593 logic for omit_dir_times / omit_link_times
        // When dir/link times are omitted, all three time-skip flags are set.
        let omit_times = AttrsFlags::SKIP_MTIME | AttrsFlags::SKIP_ATIME | AttrsFlags::SKIP_CRTIME;
        assert_eq!(omit_times, AttrsFlags::SKIP_ALL_TIMES);

        // When only mtime is not preserved, only SKIP_MTIME is set.
        let no_preserve_mtime = AttrsFlags::SKIP_MTIME;
        assert!(no_preserve_mtime.skip_mtime());
        assert!(!no_preserve_mtime.skip_atime());
        assert!(!no_preserve_mtime.skip_crtime());
    }

    #[test]
    fn attrs_flags_upstream_accurate_time_with_report() {
        // Mirrors upstream generator.c:1814 where quick-check match combines
        // maybe_ATTRS_REPORT and maybe_ATTRS_ACCURATE_TIME.
        let flags = AttrsFlags::REPORT | AttrsFlags::ACCURATE_TIME;
        assert!(flags.report());
        assert!(flags.accurate_time());
        assert!(!flags.skip_mtime());
    }

    #[test]
    fn attrs_flags_upstream_receiver_skip_all_when_not_ok_to_set_time() {
        // Mirrors upstream rsync.c:749 and :774 where ok_to_set_time is false:
        // ATTRS_SKIP_MTIME | ATTRS_SKIP_ATIME | ATTRS_SKIP_CRTIME
        let flags = AttrsFlags::SKIP_MTIME | AttrsFlags::SKIP_ATIME | AttrsFlags::SKIP_CRTIME;
        assert!(flags.skip_mtime());
        assert!(flags.skip_atime());
        assert!(flags.skip_crtime());
        assert!(!flags.accurate_time());
    }

    // ==================== MetadataOptions tests ====================

    #[test]
    fn defaults_match_expected_configuration() {
        let options = MetadataOptions::new();

        assert!(!options.owner());
        assert!(!options.group());
        assert!(!options.executability());
        assert!(options.permissions());
        assert!(options.times());
        assert!(!options.atimes());
        assert!(!options.crtimes());
        assert!(!options.numeric_ids_enabled());
        assert!(!options.fake_super_enabled());
        assert!(options.owner_override().is_none());
        assert!(options.group_override().is_none());
        assert!(options.chmod().is_none());
        assert!(options.user_mapping().is_none());
        assert!(options.group_mapping().is_none());

        assert_eq!(MetadataOptions::default(), options);
    }

    #[cfg(unix)]
    #[test]
    fn builder_methods_apply_requested_flags() {
        let modifiers = ChmodModifiers::parse("u=rw").expect("parse modifiers");

        let user_map = UserMapping::parse("1000:2000").expect("parse usermap");
        let group_map = GroupMapping::parse("*:3000").expect("parse groupmap");

        let options = MetadataOptions::new()
            .preserve_owner(true)
            .preserve_group(true)
            .preserve_executability(true)
            .preserve_permissions(false)
            .preserve_times(false)
            .numeric_ids(true)
            .with_owner_override(Some(42))
            .with_group_override(Some(7))
            .with_chmod(Some(modifiers.clone()))
            .with_user_mapping(Some(user_map.clone()))
            .with_group_mapping(Some(group_map.clone()));

        assert!(options.owner());
        assert!(options.group());
        assert!(options.executability());
        assert!(!options.permissions());
        assert!(!options.times());
        assert!(options.numeric_ids_enabled());
        assert_eq!(options.owner_override(), Some(42));
        assert_eq!(options.group_override(), Some(7));
        assert_eq!(options.chmod(), Some(&modifiers));
        assert_eq!(options.user_mapping(), Some(&user_map));
        assert_eq!(options.group_mapping(), Some(&group_map));
    }

    #[test]
    fn fake_super_can_be_enabled() {
        let options = MetadataOptions::new().fake_super(true);
        assert!(options.fake_super_enabled());

        let disabled = MetadataOptions::new().fake_super(false);
        assert!(!disabled.fake_super_enabled());
    }

    #[test]
    fn overrides_and_chmod_can_be_cleared() {
        let base = MetadataOptions::new()
            .with_owner_override(Some(13))
            .with_group_override(Some(24))
            .with_chmod(Some(ChmodModifiers::parse("g+x").expect("parse modifiers")));

        let cleared = base
            .with_owner_override(None)
            .with_group_override(None)
            .with_chmod(None)
            .preserve_owner(false)
            .preserve_group(false)
            .preserve_permissions(true)
            .preserve_times(true)
            .numeric_ids(false);

        assert!(!cleared.owner());
        assert!(!cleared.group());
        assert!(cleared.permissions());
        assert!(cleared.times());
        assert!(!cleared.numeric_ids_enabled());
        assert!(cleared.owner_override().is_none());
        assert!(cleared.group_override().is_none());
        assert!(cleared.chmod().is_none());
    }
}
