use compress::zlib::{CompressionLevel, CompressionLevelError};

/// Compression configuration propagated from the CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum CompressionSetting {
    /// Compression has been explicitly disabled (e.g. `--compress-level=0`).
    ///
    /// This is also the default when building a [`ClientConfig`](super::super::ClientConfig),
    /// matching upstream rsync's behaviour of leaving compression off unless the
    /// caller explicitly enables it.
    #[default]
    Disabled,
    /// Compression is enabled with the provided [`CompressionLevel`].
    Level(CompressionLevel),
}

impl CompressionSetting {
    /// Returns a setting that disables compression.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    /// Returns a setting that enables compression using `level`.
    #[must_use]
    pub const fn level(level: CompressionLevel) -> Self {
        Self::Level(level)
    }

    /// Parses a numeric compression level into a [`CompressionSetting`].
    ///
    /// Values `1` through `9` map to [`CompressionLevel::Precise`]. A value of
    /// `0` disables compression, mirroring upstream rsync's interpretation of
    /// `--compress-level=0`. Values outside the supported range return
    /// [`CompressionLevelError`].
    pub fn try_from_numeric(level: u32) -> Result<Self, CompressionLevelError> {
        if level == 0 {
            Ok(Self::Disabled)
        } else {
            CompressionLevel::from_numeric(level).map(Self::Level)
        }
    }

    /// Reports whether compression should be enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Level(_))
    }

    /// Reports whether compression has been explicitly disabled.
    #[must_use]
    pub const fn is_disabled(self) -> bool {
        !self.is_enabled()
    }

    /// Returns the compression level that should be used when compression is
    /// enabled. When compression is disabled the default zlib level is
    /// returned, mirroring upstream rsync's behaviour when the caller toggles
    /// compression without specifying an explicit level.
    #[must_use]
    pub const fn level_or_default(self) -> CompressionLevel {
        match self {
            Self::Level(level) => level,
            Self::Disabled => CompressionLevel::Default,
        }
    }
}

impl From<CompressionLevel> for CompressionSetting {
    fn from(level: CompressionLevel) -> Self {
        Self::Level(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        assert_eq!(CompressionSetting::default(), CompressionSetting::Disabled);
    }

    #[test]
    fn disabled_constructor() {
        assert_eq!(CompressionSetting::disabled(), CompressionSetting::Disabled);
    }

    #[test]
    fn level_constructor() {
        let level = CompressionLevel::Default;
        let setting = CompressionSetting::level(level);
        assert!(matches!(setting, CompressionSetting::Level(_)));
    }

    #[test]
    fn try_from_numeric_zero_disables() {
        let result = CompressionSetting::try_from_numeric(0).unwrap();
        assert_eq!(result, CompressionSetting::Disabled);
    }

    #[test]
    fn try_from_numeric_valid_levels() {
        for level in 1..=9 {
            let result = CompressionSetting::try_from_numeric(level);
            assert!(result.is_ok());
            assert!(result.unwrap().is_enabled());
        }
    }

    #[test]
    fn try_from_numeric_invalid_level() {
        let result = CompressionSetting::try_from_numeric(10);
        assert!(result.is_err());
    }

    #[test]
    fn is_enabled_disabled() {
        assert!(!CompressionSetting::Disabled.is_enabled());
    }

    #[test]
    fn is_enabled_level() {
        let setting = CompressionSetting::level(CompressionLevel::Default);
        assert!(setting.is_enabled());
    }

    #[test]
    fn is_disabled() {
        assert!(CompressionSetting::Disabled.is_disabled());
        assert!(!CompressionSetting::level(CompressionLevel::Default).is_disabled());
    }

    #[test]
    fn level_or_default_with_disabled() {
        let setting = CompressionSetting::Disabled;
        assert_eq!(setting.level_or_default(), CompressionLevel::Default);
    }

    #[test]
    fn from_compression_level() {
        let level = CompressionLevel::Default;
        let setting: CompressionSetting = level.into();
        assert!(matches!(setting, CompressionSetting::Level(_)));
    }
}
