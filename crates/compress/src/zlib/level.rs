//! Compression level types and validation for the zlib encoder.

use std::num::NonZeroU8;

use flate2::Compression;
use thiserror::Error;

// upstream: token.c:init_compression_level() - zlib range 0..=9,
// default 6 (Z_DEFAULT_COMPRESSION is -1 but upstream remaps to 6).
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
    /// Use an explicit signed codec level that a [`NonZeroU8`] cannot express.
    ///
    /// This exists for zstd, whose valid `--compress-level` range extends below
    /// zero down to `ZSTD_minCLevel()`. Upstream passes those negative "fast"
    /// levels straight to `ZSTD_c_compressionLevel` (token.c:73,748), so they
    /// must survive end to end rather than being collapsed into the unsigned
    /// [`Precise`](Self::Precise) range. Never produced for zlib.
    PreciseSigned(i32),
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

    /// Builds a level from an already-range-checked signed codec level.
    ///
    /// Used by the zstd clamp path, whose valid range includes negative "fast"
    /// levels down to `ZSTD_minCLevel()`. A positive level (`1..=255`) becomes
    /// [`Precise`](Self::Precise); any value a [`NonZeroU8`] cannot hold (i.e. a
    /// negative one) becomes [`PreciseSigned`](Self::PreciseSigned), preserving
    /// the sign so it reaches `ZSTD_c_compressionLevel` unchanged.
    #[must_use]
    pub fn from_signed(level: i32) -> Self {
        match u8::try_from(level).ok().and_then(NonZeroU8::new) {
            Some(value) => Self::Precise(value),
            None => Self::PreciseSigned(level),
        }
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
            // zlib never yields a signed level; clamp defensively into zlib's
            // valid 0..=9 range so this arm can never panic in flate2.
            CompressionLevel::PreciseSigned(value) => Compression::new(value.clamp(0, 9) as u32),
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
