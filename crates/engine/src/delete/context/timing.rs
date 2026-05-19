//! [`EmitterTiming`] - the four upstream `--delete-*` timing modes and
//! conversions to/from the engine's [`crate::local_copy::DeleteTiming`].

/// Re-exposes the four upstream timing modes so the emitter and its
/// context can be configured without pulling in the engine's
/// `LocalCopyOptions` type. The variants match
/// [`crate::local_copy::DeleteTiming`] one-for-one; conversion is
/// provided via [`From`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EmitterTiming {
    /// Run the drain before any content transfer.
    Before,
    /// Run the drain interleaved with content transfer, one directory
    /// at a time, before each directory's per-file copies.
    During,
    /// Accumulate plans during transfer; drain after all transfers
    /// complete.
    After,
    /// Accumulate plans during transfer; drain after all renames have
    /// committed.
    Delay,
}

impl EmitterTiming {
    /// Returns `true` for timing modes that drain inside the per-directory
    /// copy loop (only `During`).
    #[must_use]
    pub const fn drains_per_directory(self) -> bool {
        matches!(self, Self::During)
    }

    /// Returns `true` for timing modes that drain after every transfer
    /// (`After` and `Delay`).
    #[must_use]
    pub const fn drains_post_transfer(self) -> bool {
        matches!(self, Self::After | Self::Delay)
    }

    /// Returns `true` for timing modes that drain before any transfer
    /// (only `Before`).
    #[must_use]
    pub const fn drains_pre_transfer(self) -> bool {
        matches!(self, Self::Before)
    }
}

impl From<crate::local_copy::DeleteTiming> for EmitterTiming {
    fn from(value: crate::local_copy::DeleteTiming) -> Self {
        match value {
            crate::local_copy::DeleteTiming::Before => Self::Before,
            crate::local_copy::DeleteTiming::During => Self::During,
            crate::local_copy::DeleteTiming::After => Self::After,
            crate::local_copy::DeleteTiming::Delay => Self::Delay,
        }
    }
}

impl From<EmitterTiming> for crate::local_copy::DeleteTiming {
    fn from(value: EmitterTiming) -> Self {
        match value {
            EmitterTiming::Before => Self::Before,
            EmitterTiming::During => Self::During,
            EmitterTiming::After => Self::After,
            EmitterTiming::Delay => Self::Delay,
        }
    }
}
