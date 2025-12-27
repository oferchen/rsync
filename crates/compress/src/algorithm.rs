//! Shared enumeration describing compression algorithms supported by the workspace.

use ::core::str::FromStr;

use thiserror::Error;

/// Compression algorithms recognised by the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CompressionAlgorithm {
    /// Classic zlib/deflate compression.
    Zlib,
    /// LZ4 frame compression (`--compress-choice=lz4`).
    #[cfg(feature = "lz4")]
    Lz4,
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
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => "lz4",
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
        #[cfg(all(feature = "zstd", feature = "lz4"))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] = &[
                CompressionAlgorithm::Zlib,
                CompressionAlgorithm::Lz4,
                CompressionAlgorithm::Zstd,
            ];
            ALGORITHMS
        }

        #[cfg(all(feature = "zstd", not(feature = "lz4")))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] =
                &[CompressionAlgorithm::Zlib, CompressionAlgorithm::Zstd];
            ALGORITHMS
        }

        #[cfg(all(feature = "lz4", not(feature = "zstd")))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] =
                &[CompressionAlgorithm::Zlib, CompressionAlgorithm::Lz4];
            ALGORITHMS
        }

        #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
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

    #[cfg(feature = "lz4")]
    #[test]
    fn available_algorithms_include_lz4_when_feature_enabled() {
        let available = CompressionAlgorithm::available();
        assert!(available.contains(&CompressionAlgorithm::Lz4));
    }

    #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
    #[test]
    fn available_algorithms_only_include_zlib_when_no_optional_features_enabled() {
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

    #[cfg(feature = "lz4")]
    #[test]
    fn parsing_accepts_lz4() {
        assert_eq!(
            "lz4".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Lz4
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

    #[test]
    fn compression_algorithm_name_zlib() {
        assert_eq!(CompressionAlgorithm::Zlib.name(), "zlib");
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn compression_algorithm_name_lz4() {
        assert_eq!(CompressionAlgorithm::Lz4.name(), "lz4");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn compression_algorithm_name_zstd() {
        assert_eq!(CompressionAlgorithm::Zstd.name(), "zstd");
    }

    #[test]
    fn default_algorithm_is_zlib() {
        assert_eq!(
            CompressionAlgorithm::default_algorithm(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(CompressionAlgorithm::default(), CompressionAlgorithm::Zlib);
    }

    #[test]
    fn compression_algorithm_clone() {
        let algo = CompressionAlgorithm::Zlib;
        let cloned = algo;
        assert_eq!(algo, cloned);
    }

    #[test]
    fn compression_algorithm_copy() {
        let algo = CompressionAlgorithm::Zlib;
        let copied = algo;
        assert_eq!(algo, copied);
    }

    #[test]
    fn compression_algorithm_debug() {
        let debug = format!("{:?}", CompressionAlgorithm::Zlib);
        assert!(debug.contains("Zlib"));
    }

    #[test]
    fn compression_algorithm_eq() {
        assert_eq!(CompressionAlgorithm::Zlib, CompressionAlgorithm::Zlib);
    }

    #[test]
    fn compression_algorithm_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(CompressionAlgorithm::Zlib);
        assert!(set.contains(&CompressionAlgorithm::Zlib));
    }

    #[test]
    fn parsing_trims_whitespace() {
        assert_eq!(
            "  zlib  ".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
    }

    #[test]
    fn parsing_case_insensitive() {
        assert_eq!(
            "ZLIB".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(
            "ZlIb".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
    }

    #[test]
    fn parse_error_new() {
        let error = CompressionAlgorithmParseError::new("test");
        assert_eq!(error.input(), "test");
    }

    #[test]
    fn parse_error_display() {
        let error = CompressionAlgorithmParseError::new("invalid");
        let display = error.to_string();
        assert!(display.contains("invalid"));
        assert!(display.contains("unsupported"));
    }

    #[test]
    fn parse_error_debug() {
        let error = CompressionAlgorithmParseError::new("test");
        let debug = format!("{error:?}");
        assert!(debug.contains("CompressionAlgorithmParseError"));
    }

    #[test]
    fn parse_error_eq() {
        let a = CompressionAlgorithmParseError::new("test");
        let b = CompressionAlgorithmParseError::new("test");
        let c = CompressionAlgorithmParseError::new("other");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn parse_error_clone() {
        let error = CompressionAlgorithmParseError::new("test");
        let cloned = error.clone();
        assert_eq!(error, cloned);
    }

    #[test]
    fn available_is_not_empty() {
        assert!(!CompressionAlgorithm::available().is_empty());
    }
}

/// Error returned when attempting to parse an unsupported compression algorithm.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("unsupported compression algorithm: {input}")]
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

impl FromStr for CompressionAlgorithm {
    type Err = CompressionAlgorithmParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zlib" | "zlibx" => Ok(CompressionAlgorithm::Zlib),
            #[cfg(feature = "lz4")]
            "lz4" => Ok(CompressionAlgorithm::Lz4),
            #[cfg(feature = "zstd")]
            "zstd" => Ok(CompressionAlgorithm::Zstd),
            other => Err(CompressionAlgorithmParseError::new(other.to_string())),
        }
    }
}
