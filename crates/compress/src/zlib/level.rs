//! Compression level types and validation for the zlib encoder.

use std::num::NonZeroU8;

use flate2::Compression;
use thiserror::Error;

/// Compression levels recognised by the zlib encoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionLevel {
    /// No compression (level 0) - data is stored without deflation.
    None,
    /// Favour speed over compression ratio.
    Fast,
    /// Use zlib's default balance between speed and ratio.
    Default,
    /// Favour the best possible compression ratio.
    Best,
    /// Use an explicit zlib compression level in the range `1..=9`.
    Precise(NonZeroU8),
}

impl CompressionLevel {
    /// Creates a [`CompressionLevel`] value from an explicit numeric level.
    ///
    /// Level 0 returns [`CompressionLevel::None`] (no compression).
    /// Levels 1-9 return [`CompressionLevel::Precise`].
    ///
    /// # Errors
    ///
    /// Returns [`CompressionLevelError`] when `level` falls outside the inclusive
    /// range `0..=9` accepted by zlib.
    pub fn from_numeric(level: u32) -> Result<Self, CompressionLevelError> {
        if level > 9 {
            return Err(CompressionLevelError::new(level));
        }

        if level == 0 {
            return Ok(Self::None);
        }

        let as_u8 = u8::try_from(level).map_err(|_| CompressionLevelError::new(level))?;
        let precise = NonZeroU8::new(as_u8).ok_or_else(|| CompressionLevelError::new(level))?;
        Ok(Self::Precise(precise))
    }

    /// Constructs a [`CompressionLevel::Precise`] variant from the provided zlib level.
    #[must_use]
    pub const fn precise(level: NonZeroU8) -> Self {
        Self::Precise(level)
    }
}

impl From<CompressionLevel> for Compression {
    fn from(level: CompressionLevel) -> Self {
        match level {
            CompressionLevel::None => Compression::none(),
            CompressionLevel::Fast => Compression::fast(),
            CompressionLevel::Default => Compression::default(),
            CompressionLevel::Best => Compression::best(),
            CompressionLevel::Precise(value) => Compression::new(u32::from(value.get())),
        }
    }
}

/// Error returned when a requested compression level falls outside the
/// permissible zlib range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("compression level {level} is outside the supported range 0-9")]
pub struct CompressionLevelError {
    level: u32,
}

impl CompressionLevelError {
    /// Creates a new error capturing the unsupported compression level.
    pub(crate) const fn new(level: u32) -> Self {
        Self { level }
    }

    /// Returns the invalid compression level that triggered the error.
    #[must_use]
    pub const fn level(&self) -> u32 {
        self.level
    }
}
