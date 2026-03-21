use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

use crate::zlib::{CompressionLevel, CountingZlibEncoder};

use super::magic::KNOWN_SIGNATURES;
use super::types::{CompressionDecision, FileCategory};
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
    skip_extensions: HashSet<String>,
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
        let mut decider = Self::new();
        decider.add_default_skip_extensions();
        decider
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

    /// Adds the default set of incompressible file extensions.
    ///
    /// This list is based on upstream rsync's default skip-compress list.
    pub fn add_default_skip_extensions(&mut self) {
        // Images
        for ext in &[
            "jpg", "jpeg", "jpe", "png", "gif", "webp", "heic", "heif", "avif", "tif", "tiff",
            "bmp", "ico", "svg", "svgz", "psd", "raw", "arw", "cr2", "nef", "orf", "sr2",
        ] {
            self.skip_extensions.insert((*ext).to_owned());
        }

        // Video
        for ext in &[
            "mp4", "m4v", "mkv", "avi", "mov", "wmv", "flv", "webm", "mpeg", "mpg", "vob", "ogv",
            "3gp", "3g2", "ts", "mts", "m2ts",
        ] {
            self.skip_extensions.insert((*ext).to_owned());
        }

        // Audio
        for ext in &[
            "mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wma", "wav", "aiff", "ape", "mka",
            "ac3", "dts",
        ] {
            self.skip_extensions.insert((*ext).to_owned());
        }

        // Archives and compressed files
        for ext in &[
            "zip", "gz", "gzip", "bz2", "bzip2", "xz", "lzma", "7z", "rar", "zst", "zstd", "lz4",
            "lzo", "z", "cab", "arj", "lzh", "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "tgz",
            "tbz", "tbz2", "txz",
        ] {
            self.skip_extensions.insert((*ext).to_owned());
        }

        // Package formats (pre-compressed)
        for ext in &[
            "deb", "rpm", "apk", "jar", "war", "ear", "egg", "whl", "gem", "nupkg", "snap", "appx",
            "msix",
        ] {
            self.skip_extensions.insert((*ext).to_owned());
        }

        // Documents (often pre-compressed)
        for ext in &[
            "pdf", "epub", "mobi", "azw", "azw3", "docx", "xlsx", "pptx", "odt", "ods", "odp",
        ] {
            self.skip_extensions.insert((*ext).to_owned());
        }

        // Disk images (often compressed or encrypted)
        for ext in &["iso", "img", "dmg", "vhd", "vhdx", "vmdk", "qcow", "qcow2"] {
            self.skip_extensions.insert((*ext).to_owned());
        }
    }

    /// Adds a single extension to the skip list.
    ///
    /// The extension is normalized: leading dots are stripped and it's converted to lowercase.
    pub fn add_skip_extension(&mut self, ext: &str) {
        let normalized = ext.trim_start_matches('.').trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            self.skip_extensions.insert(normalized);
        }
    }

    /// Removes an extension from the skip list.
    pub fn remove_skip_extension(&mut self, ext: &str) -> bool {
        let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
        self.skip_extensions.remove(&normalized)
    }

    /// Returns the current set of skip extensions.
    #[must_use]
    pub fn skip_extensions(&self) -> &HashSet<String> {
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

    /// Returns the current compression threshold.
    #[must_use]
    pub fn compression_threshold(&self) -> f64 {
        self.compression_threshold
    }

    /// Sets the sample size for auto-detection.
    pub fn set_sample_size(&mut self, size: usize) {
        self.sample_size = size.max(64); // Minimum 64 bytes for meaningful detection
    }

    /// Returns the current sample size.
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
        // Check extension first (fastest path)
        if let Some(ext) = Self::extract_extension(path) {
            if self.skip_extensions.contains(&ext) {
                return CompressionDecision::Skip;
            }
        }

        // Check magic bytes if available and enabled
        if self.use_magic_detection {
            if let Some(data) = first_block {
                if let Some(category) = self.detect_category_by_magic(data) {
                    if !category.is_compressible() {
                        return CompressionDecision::Skip;
                    }
                }
            }
        }

        // If we have content but couldn't determine the type, suggest auto-detection
        if first_block.is_some() {
            return CompressionDecision::Compress;
        }

        // Without content, we can't make a definitive decision
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
            return Ok(true); // Empty files are trivially compressible
        }

        // Use fast compression level for auto-detection
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Fast);
        encoder.write_all(sample)?;
        let compressed_len = encoder.finish()? as usize;

        // Calculate compression ratio
        let ratio = compressed_len as f64 / sample.len() as f64;

        Ok(ratio < self.compression_threshold)
    }

    /// Extracts and normalizes the file extension from a path.
    pub(crate) fn extract_extension(path: &Path) -> Option<String> {
        // Handle compound extensions like .tar.gz
        let file_name = path.file_name()?.to_str()?;
        let lower = file_name.to_ascii_lowercase();

        // Check for known compound extensions
        for compound in &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
            if lower.ends_with(compound) {
                return Some(compound.trim_start_matches('.').to_owned());
            }
        }

        // Fall back to simple extension
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
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
