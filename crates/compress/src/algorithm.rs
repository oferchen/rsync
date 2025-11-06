//! Shared enumeration describing compression algorithms supported by the workspace.

use core::fmt;
use core::str::FromStr;

/// Compression algorithms recognised by the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CompressionAlgorithm {
    /// Classic zlib/deflate compression.
    Zlib,
    /// Zstandard compression (`--compress-choice=zstd`).
    #[cfg(feature = "zstd")]
    Zstd,
}

impl CompressionAlgorithm {
    /// Returns the canonical display name used for version output and diagnostics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CompressionAlgorithm::Zlib => "zlib",
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => "zstd",
        }
    }

    /// Returns the default compression algorithm used when callers enable `--compress`.
    #[must_use]
    pub const fn default_algorithm() -> Self {
        CompressionAlgorithm::Zlib
    }

    /// Returns the set of algorithms available in the current build.
    #[must_use]
    pub fn available() -> &'static [CompressionAlgorithm] {
        #[cfg(feature = "zstd")]
        {
            const ALGORITHMS: &[CompressionAlgorithm] =
                &[CompressionAlgorithm::Zlib, CompressionAlgorithm::Zstd];
            ALGORITHMS
        }

        #[cfg(not(feature = "zstd"))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] = &[CompressionAlgorithm::Zlib];
            ALGORITHMS
        }
    }
}

impl Default for CompressionAlgorithm {
    fn default() -> Self {
        Self::default_algorithm()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_algorithms_always_include_zlib() {
        let available = CompressionAlgorithm::available();
        assert!(available.contains(&CompressionAlgorithm::Zlib));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn available_algorithms_include_zstd_when_feature_enabled() {
        let available = CompressionAlgorithm::available();
        assert!(available.contains(&CompressionAlgorithm::Zstd));
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn available_algorithms_exclude_zstd_when_feature_disabled() {
        let available = CompressionAlgorithm::available();
        assert_eq!(available, &[CompressionAlgorithm::Zlib]);
    }

    #[test]
    fn parsing_accepts_known_algorithms() {
        assert_eq!(
            "zlib".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(
            "zlibx".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn parsing_accepts_zstd() {
        assert_eq!(
            "zstd".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zstd
        );
    }

    #[test]
    fn parsing_rejects_unknown_algorithms() {
        let err = "brotli"
            .parse::<CompressionAlgorithm>()
            .expect_err("brotli unsupported");
        assert_eq!(err.input(), "brotli");
    }
}

/// Error returned when attempting to parse an unsupported compression algorithm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressionAlgorithmParseError {
    input: String,
}

impl CompressionAlgorithmParseError {
    /// Creates a parse error capturing the original input.
    #[must_use]
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
        }
    }

    /// Returns the invalid input.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for CompressionAlgorithmParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported compression algorithm: {}", self.input)
    }
}

impl std::error::Error for CompressionAlgorithmParseError {}

impl FromStr for CompressionAlgorithm {
    type Err = CompressionAlgorithmParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zlib" | "zlibx" => Ok(CompressionAlgorithm::Zlib),
            #[cfg(feature = "zstd")]
            "zstd" => Ok(CompressionAlgorithm::Zstd),
            other => Err(CompressionAlgorithmParseError::new(other.to_string())),
        }
    }
}
