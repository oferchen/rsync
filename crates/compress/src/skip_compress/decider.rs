//! Compression decision engine.
//!
//! Implements the three-pronged approach for deciding whether to compress a
//! file: extension lookup, magic byte detection, and sample-based auto-detection.
//! The extension-based skip list mirrors upstream rsync's `--skip-compress` option
//! and `lp_dont_compress()` default list.
//!
//! # Upstream Reference
//!
//! See `token.c:set_compression()` and `token.c:init_set_compression()` for the
//! upstream suffix tree and wildcard matching that drives per-file compression
//! level selection.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

use crate::zlib::{CompressionLevel, CountingZlibEncoder};

use super::defaults::{self, COMPOUND_EXTENSIONS};
use super::magic::KNOWN_SIGNATURES;
use super::types::{CompressionDecision, FileCategory, Suffix};
use super::{DEFAULT_COMPRESSION_THRESHOLD, DEFAULT_SAMPLE_SIZE};

/// Compression decision engine based on file type and content analysis.
///
/// The decider maintains a set of file extensions that should skip compression,
/// based on the upstream rsync `--skip-compress` option behavior.
///
/// It uses a three-pronged approach:
/// 1. Extension-based detection - fast O(1) lookup for known file extensions
/// 2. Magic byte detection - identify compressed files by their headers
/// 3. Auto-detection - sample-based compression ratio analysis
#[derive(Clone, Debug)]
pub struct CompressionDecider {
    /// Extensions to skip (lowercase, without leading dot).
    skip_extensions: HashSet<Suffix>,
    /// Compression ratio threshold for auto-detection.
    compression_threshold: f64,
    /// Sample size for auto-detection.
    sample_size: usize,
    /// Whether to use magic byte detection.
    use_magic_detection: bool,
}

impl Default for CompressionDecider {
    fn default() -> Self {
        Self::with_default_skip_list()
    }
}

impl CompressionDecider {
    /// Creates a new compression decider with no skip extensions.
    #[must_use]
    pub fn new() -> Self {
        Self {
            skip_extensions: HashSet::new(),
            compression_threshold: DEFAULT_COMPRESSION_THRESHOLD,
            sample_size: DEFAULT_SAMPLE_SIZE,
            use_magic_detection: true,
        }
    }

    /// Creates a compression decider with the default skip list.
    ///
    /// The default list matches upstream rsync's built-in list of file extensions
    /// that typically don't benefit from compression.
    #[must_use]
    pub fn with_default_skip_list() -> Self {
        Self {
            skip_extensions: defaults::default_skip_extensions(),
            compression_threshold: DEFAULT_COMPRESSION_THRESHOLD,
            sample_size: DEFAULT_SAMPLE_SIZE,
            use_magic_detection: true,
        }
    }

    /// Creates a compression decider from a user-provided skip list.
    ///
    /// The list should be a comma or space separated string of file extensions
    /// (with or without leading dots), matching upstream rsync's `--skip-compress` format.
    #[must_use]
    pub fn from_skip_compress_list(list: &str) -> Self {
        let mut decider = Self::new();
        decider.parse_skip_compress_list(list);
        decider
    }

    /// Parses a skip-compress list string and adds extensions to the skip set.
    ///
    /// The format matches upstream rsync's `--skip-compress` option:
    /// - Extensions are separated by `/` or whitespace
    /// - Leading dots are optional
    /// - Extensions are case-insensitive
    pub fn parse_skip_compress_list(&mut self, list: &str) {
        for ext in list.split(|c: char| c == '/' || c.is_whitespace()) {
            let ext = ext.trim();
            if !ext.is_empty() {
                self.add_skip_extension(ext);
            }
        }
    }

    /// Adds a single extension to the skip list.
    ///
    /// The extension is normalized: leading dots are stripped and it's converted to lowercase.
    pub fn add_skip_extension(&mut self, ext: &str) {
        let suffix = Suffix::new(ext);
        if !suffix.is_empty() {
            self.skip_extensions.insert(suffix);
        }
    }

    /// Removes an extension from the skip list.
    pub fn remove_skip_extension(&mut self, ext: &str) -> bool {
        let suffix = Suffix::new(ext);
        self.skip_extensions.remove(&suffix)
    }

    /// File extensions that bypass compression entirely.
    #[must_use]
    pub fn skip_extensions(&self) -> &HashSet<Suffix> {
        &self.skip_extensions
    }

    /// Clears all skip extensions.
    pub fn clear_skip_extensions(&mut self) {
        self.skip_extensions.clear();
    }

    /// Sets the compression ratio threshold for auto-detection.
    ///
    /// Files where compressed_size / original_size >= threshold are considered incompressible.
    pub fn set_compression_threshold(&mut self, threshold: f64) {
        self.compression_threshold = threshold.clamp(0.0, 1.0);
    }

    /// Ratio at or above which a file is considered incompressible.
    #[must_use]
    pub fn compression_threshold(&self) -> f64 {
        self.compression_threshold
    }

    /// Sets the sample size for auto-detection.
    pub fn set_sample_size(&mut self, size: usize) {
        // 64 bytes is the smallest sample that yields meaningful ratio measurements.
        self.sample_size = size.max(64);
    }

    /// Byte count sampled from each file for auto-detection.
    #[must_use]
    pub fn sample_size(&self) -> usize {
        self.sample_size
    }

    /// Enables or disables magic byte detection.
    pub fn set_use_magic_detection(&mut self, enable: bool) {
        self.use_magic_detection = enable;
    }

    /// Returns whether magic byte detection is enabled.
    #[must_use]
    pub fn use_magic_detection(&self) -> bool {
        self.use_magic_detection
    }

    /// Determines whether a file should be compressed based on its path and optional content.
    ///
    /// Returns `Skip` for known incompressible extensions and magic byte matches,
    /// `Compress` when content is provided and no skip criteria match, or
    /// `AutoDetect` when no content is available for analysis.
    #[must_use]
    pub fn should_compress(&self, path: &Path, first_block: Option<&[u8]>) -> CompressionDecision {
        // Extension lookup is the fastest path; check it before any content inspection.
        if let Some(ext) = Self::extract_extension(path) {
            if self.skip_extensions.contains(ext.as_str()) {
                return CompressionDecision::Skip;
            }
        }

        if self.use_magic_detection {
            if let Some(data) = first_block {
                if let Some(category) = self.detect_category_by_magic(data) {
                    if !category.is_compressible() {
                        return CompressionDecision::Skip;
                    }
                }
            }
        }

        if first_block.is_some() {
            return CompressionDecision::Compress;
        }

        // No content available; defer to caller's auto-detection pass.
        CompressionDecision::AutoDetect
    }

    /// Performs auto-detection by compressing a sample and checking the ratio.
    ///
    /// Returns `Ok(true)` if the sample compresses well (should compress the file),
    /// `Ok(false)` if the sample doesn't compress well (should skip compression).
    ///
    /// # Example
    ///
    /// ```
    /// use compress::skip_compress::CompressionDecider;
    ///
    /// let decider = CompressionDecider::new();
    ///
    /// // Repetitive data compresses well
    /// let repetitive_data = vec![b'a'; 4096];
    /// assert!(decider.auto_detect_compressible(&repetitive_data).unwrap());
    /// ```
    pub fn auto_detect_compressible(&self, sample: &[u8]) -> io::Result<bool> {
        if sample.is_empty() {
            // Empty input has nothing to skip and is trivially "compressible".
            return Ok(true);
        }

        // Use the fastest level: detection only needs ratio, not optimal output.
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Fast);
        encoder.write_all(sample)?;
        let compressed_len = encoder.finish()? as usize;

        let ratio = compressed_len as f64 / sample.len() as f64;

        Ok(ratio < self.compression_threshold)
    }

    /// Extracts and normalizes the file extension from a path.
    pub(crate) fn extract_extension(path: &Path) -> Option<Suffix> {
        let file_name = path.file_name()?.to_str()?;
        let lower = file_name.to_ascii_lowercase();

        // Compound suffixes like `.tar.gz` must match the full filename to win
        // over the trailing simple extension (`gz`).
        for compound in COMPOUND_EXTENSIONS {
            if lower.ends_with(compound) {
                return Some(Suffix::new(compound.trim_start_matches('.')));
            }
        }

        path.extension()
            .and_then(|ext| ext.to_str())
            .map(Suffix::new)
    }

    /// Detects file category by examining magic bytes.
    fn detect_category_by_magic(&self, data: &[u8]) -> Option<FileCategory> {
        for sig in KNOWN_SIGNATURES {
            if sig.matches(data) {
                // Special handling for RIFF container (can be AVI, WAV, or WEBP)
                if sig.bytes == b"RIFF" {
                    if data.len() >= 12 {
                        let fourcc = &data[8..12];
                        return Some(match fourcc {
                            b"AVI " => FileCategory::Video,
                            b"WAVE" => FileCategory::Audio,
                            b"WEBP" => FileCategory::Image,
                            _ => continue, // Unknown RIFF subtype, check other signatures
                        });
                    } else {
                        // Incomplete RIFF header - not enough data to determine type
                        continue;
                    }
                }

                return Some(sig.category);
            }
        }
        None
    }
}
