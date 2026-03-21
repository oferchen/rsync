//! Bitflags controlling time-related attribute application.
//!
//! Mirrors upstream rsync's `ATTRS_*` constants from `rsync.h:192-196`.

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
    /// Upstream: `rsync.h:192` - `ATTRS_REPORT (1<<0)`
    pub const REPORT: Self = Self(1 << 0);

    /// Skip modification time comparison and application entirely. When set,
    /// `set_file_attrs` will not touch the destination's mtime regardless of
    /// whether `preserve_times` is enabled.
    ///
    /// Upstream: `rsync.h:193` - `ATTRS_SKIP_MTIME (1<<1)`
    pub const SKIP_MTIME: Self = Self(1 << 1);

    /// Force exact time comparison instead of using `modify_window` tolerance.
    /// Used after a transfer or checksum verification where the timestamp was
    /// just set and should match precisely.
    ///
    /// Upstream: `rsync.h:194` - `ATTRS_ACCURATE_TIME (1<<2)`
    pub const ACCURATE_TIME: Self = Self(1 << 2);

    /// Skip access time comparison and application entirely. When set,
    /// `set_file_attrs` will not touch the destination's atime regardless of
    /// whether `preserve_atimes` is enabled.
    ///
    /// Upstream: `rsync.h:195` - `ATTRS_SKIP_ATIME (1<<3)`
    pub const SKIP_ATIME: Self = Self(1 << 3);

    /// Skip creation time comparison and application entirely. When set,
    /// `set_file_attrs` will not touch the destination's crtime regardless of
    /// whether `preserve_crtimes` is enabled.
    ///
    /// Upstream: `rsync.h:196` - `ATTRS_SKIP_CRTIME (1<<5)`
    ///
    /// Note: bit 4 is unused in upstream, matching the gap between
    /// `ATTRS_SKIP_ATIME (1<<3)` and `ATTRS_SKIP_CRTIME (1<<5)`.
    pub const SKIP_CRTIME: Self = Self(1 << 5);

    /// Convenience constant combining all three time-skip flags.
    ///
    /// Used when directory times or link times should be omitted entirely.
    ///
    /// Upstream: `rsync.c:585` - `flags |= ATTRS_SKIP_MTIME | ATTRS_SKIP_ATIME | ATTRS_SKIP_CRTIME`
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
