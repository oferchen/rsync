//! Compression tuning based on file type.
//!
//! This module implements intelligent compression decisions to avoid wasting CPU
//! on files that are already compressed or otherwise incompressible.
//!
//! # Design
//!
//! The compression decider uses a three-pronged approach:
//!
//! 1. **Extension-based detection**: Fast O(1) lookup for known file extensions
//! 2. **Magic byte detection**: Identify compressed files by their headers
//! 3. **Auto-detection**: Sample-based compression ratio analysis
//!
//! # Upstream Compatibility
//!
//! This module mirrors upstream rsync's `--skip-compress` functionality, which
//! allows users to specify file extensions that should not be compressed during
//! transfer. The default skip list includes common compressed formats.
//!
//! # Example
//!
//! ```
//! use compress::skip_compress::{CompressionDecider, CompressionDecision};
//! use std::path::Path;
//!
//! let decider = CompressionDecider::with_default_skip_list();
//!
//! // JPEG files are known to be incompressible
//! assert_eq!(
//!     decider.should_compress(Path::new("photo.jpg"), None),
//!     CompressionDecision::Skip
//! );
//!
//! // Unknown extensions without content return AutoDetect
//! assert_eq!(
//!     decider.should_compress(Path::new("document.txt"), None),
//!     CompressionDecision::AutoDetect
//! );
//!
//! // With content, the decider can make a decision
//! let text_content = b"Hello, world! This is some text.";
//! assert_eq!(
//!     decider.should_compress(Path::new("document.txt"), Some(text_content)),
//!     CompressionDecision::Compress
//! );
//! ```

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

use crate::zlib::{CompressionLevel, CountingZlibEncoder};

/// Default size of the sample block for auto-detection (4 KB).
///
/// This size provides a good balance between detection accuracy and overhead.
/// Smaller samples may miss compression patterns, while larger samples waste CPU.
pub const DEFAULT_SAMPLE_SIZE: usize = 4 * 1024;

/// Default compression ratio threshold for auto-detection.
///
/// If the compressed size is >= 90% of the original size, the file is considered
/// incompressible. This threshold matches typical behavior where compression
/// overhead means ratios above 90% provide negligible benefit.
pub const DEFAULT_COMPRESSION_THRESHOLD: f64 = 0.90;

/// Decision about whether to compress a file during transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CompressionDecision {
    /// The file should be compressed during transfer.
    Compress,
    /// The file should be transferred without compression.
    Skip,
    /// Compression decision should be made by sampling the file content.
    ///
    /// This is returned when the file extension is not in any known list
    /// and the caller should use auto-detection.
    AutoDetect,
}

/// File type categories for compression decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum FileCategory {
    /// Image files (jpg, png, gif, etc.)
    Image,
    /// Video files (mp4, mkv, avi, etc.)
    Video,
    /// Audio files (mp3, flac, ogg, etc.)
    Audio,
    /// Archive files (zip, gz, bz2, etc.)
    Archive,
    /// Document files that may be compressed (pdf, docx, etc.)
    Document,
    /// Executable files (exe, dll, so)
    Executable,
    /// Source code and text files
    Text,
    /// Data files (json, xml, csv)
    Data,
    /// Unknown file type
    Unknown,
}

impl FileCategory {
    /// Returns whether this category typically benefits from compression.
    #[must_use]
    pub const fn is_compressible(self) -> bool {
        match self {
            Self::Image | Self::Video | Self::Audio | Self::Archive => false,
            Self::Document => false, // PDFs and Office docs are usually pre-compressed
            Self::Text | Self::Data | Self::Executable => true,
            Self::Unknown => true, // Optimistic default
        }
    }
}

/// Magic byte signatures for detecting compressed file formats.
///
/// Each entry contains the byte offset and the expected bytes at that offset.
#[derive(Clone, Debug)]
pub struct MagicSignature {
    /// Offset from the start of the file
    pub offset: usize,
    /// Expected bytes at the offset
    pub bytes: &'static [u8],
    /// Category this signature identifies
    pub category: FileCategory,
}

impl MagicSignature {
    /// Creates a new magic signature.
    #[must_use]
    pub const fn new(offset: usize, bytes: &'static [u8], category: FileCategory) -> Self {
        Self {
            offset,
            bytes,
            category,
        }
    }

    /// Checks if the given data matches this signature.
    #[must_use]
    pub fn matches(&self, data: &[u8]) -> bool {
        if data.len() < self.offset + self.bytes.len() {
            return false;
        }
        &data[self.offset..self.offset + self.bytes.len()] == self.bytes
    }
}

/// Well-known magic byte signatures for compressed and media formats.
pub const KNOWN_SIGNATURES: &[MagicSignature] = &[
    // Archive formats
    MagicSignature::new(0, b"PK\x03\x04", FileCategory::Archive), // ZIP/JAR/DOCX/XLSX
    MagicSignature::new(0, b"\x1f\x8b", FileCategory::Archive),   // gzip
    MagicSignature::new(0, b"BZ", FileCategory::Archive),         // bzip2
    MagicSignature::new(0, b"\xfd7zXZ\x00", FileCategory::Archive), // xz
    MagicSignature::new(0, b"7z\xbc\xaf\x27\x1c", FileCategory::Archive), // 7z
    MagicSignature::new(0, b"Rar!\x1a\x07", FileCategory::Archive), // RAR
    MagicSignature::new(0, b"\x28\xb5\x2f\xfd", FileCategory::Archive), // zstd
    MagicSignature::new(0, b"\x04\x22\x4d\x18", FileCategory::Archive), // lz4
    // Image formats
    MagicSignature::new(0, b"\xff\xd8\xff", FileCategory::Image), // JPEG
    MagicSignature::new(0, b"\x89PNG\r\n\x1a\n", FileCategory::Image), // PNG
    MagicSignature::new(0, b"GIF87a", FileCategory::Image),       // GIF87
    MagicSignature::new(0, b"GIF89a", FileCategory::Image),       // GIF89
    MagicSignature::new(0, b"RIFF", FileCategory::Image),         // WEBP (check for WEBP later)
    MagicSignature::new(0, b"\x00\x00\x00", FileCategory::Image), // HEIC/HEIF (ftyp follows)
    // Video formats
    MagicSignature::new(0, b"\x00\x00\x00\x1c\x66\x74\x79\x70", FileCategory::Video), // MP4/MOV ftyp
    MagicSignature::new(0, b"\x00\x00\x00\x20\x66\x74\x79\x70", FileCategory::Video), // MP4 variant
    MagicSignature::new(0, b"\x1a\x45\xdf\xa3", FileCategory::Video),                 // MKV/WEBM
    MagicSignature::new(0, b"RIFF", FileCategory::Video), // AVI (check for AVI later)
    // Audio formats
    MagicSignature::new(0, b"ID3", FileCategory::Audio), // MP3 with ID3
    MagicSignature::new(0, b"\xff\xfb", FileCategory::Audio), // MP3 frame sync
    MagicSignature::new(0, b"\xff\xfa", FileCategory::Audio), // MP3 frame sync
    MagicSignature::new(0, b"fLaC", FileCategory::Audio), // FLAC
    MagicSignature::new(0, b"OggS", FileCategory::Audio), // OGG (Vorbis/Opus)
    MagicSignature::new(4, b"ftyp", FileCategory::Audio), // M4A/AAC
    MagicSignature::new(0, b"RIFF", FileCategory::Audio), // WAV (check for WAVE later)
    // Document formats
    MagicSignature::new(0, b"%PDF", FileCategory::Document), // PDF
];

/// Compression decision engine based on file type and content analysis.
///
/// The decider maintains a set of file extensions that should skip compression,
/// based on the upstream rsync `--skip-compress` option behavior.
#[derive(Clone, Debug)]
pub struct CompressionDecider {
    /// Extensions to skip (lowercase, without leading dot)
    skip_extensions: HashSet<String>,
    /// Compression ratio threshold for auto-detection
    compression_threshold: f64,
    /// Sample size for auto-detection
    sample_size: usize,
    /// Whether to use magic byte detection
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
    /// # Arguments
    ///
    /// * `path` - The file path (used for extension-based detection)
    /// * `first_block` - Optional first block of file content (used for magic byte detection)
    ///
    /// # Returns
    ///
    /// - `CompressionDecision::Skip` if the file should not be compressed
    /// - `CompressionDecision::Compress` if the file should be compressed
    /// - `CompressionDecision::AutoDetect` if the caller should sample the file content
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
    /// # Arguments
    ///
    /// * `sample` - The sample data to compress
    ///
    /// # Returns
    ///
    /// - `Ok(true)` if the sample compresses well (should compress the file)
    /// - `Ok(false)` if the sample doesn't compress well (should skip compression)
    /// - `Err` if compression failed
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
    fn extract_extension(path: &Path) -> Option<String> {
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

/// Streaming compression filter that can dynamically skip compression.
///
/// This writer wraps another writer and optionally compresses data based on
/// auto-detection results from the first block. The compressor buffers initial
/// data to make a compression decision, then either passes data through directly
/// or compresses it.
pub struct AdaptiveCompressor<W: Write> {
    inner: W,
    decider: CompressionDecider,
    buffer: Vec<u8>,
    compress_buffer: Vec<u8>,
    decision_made: bool,
    should_compress: bool,
    level: CompressionLevel,
}

impl<W: Write> AdaptiveCompressor<W> {
    /// Creates a new adaptive compressor.
    pub fn new(inner: W, decider: CompressionDecider, level: CompressionLevel) -> Self {
        let sample_size = decider.sample_size();
        Self {
            inner,
            decider,
            buffer: Vec::with_capacity(sample_size),
            compress_buffer: Vec::new(),
            decision_made: false,
            should_compress: true,
            level,
        }
    }

    /// Forces a compression decision without auto-detection.
    pub fn set_decision(&mut self, should_compress: bool) {
        self.decision_made = true;
        self.should_compress = should_compress;
    }

    /// Returns whether compression was decided to be used.
    ///
    /// Returns `None` if the decision hasn't been made yet.
    #[must_use]
    pub fn compression_enabled(&self) -> Option<bool> {
        if self.decision_made {
            Some(self.should_compress)
        } else {
            None
        }
    }

    fn make_decision(&mut self) -> io::Result<()> {
        if self.decision_made {
            return Ok(());
        }

        self.should_compress = self.decider.auto_detect_compressible(&self.buffer)?;
        self.decision_made = true;

        // Flush buffered data according to decision
        if self.should_compress {
            // Compress buffered data
            let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), self.level);
            encoder.write_all(&self.buffer)?;
            let (compressed, _) = encoder.finish_into_inner()?;
            self.compress_buffer = compressed;
        } else {
            // Write buffered data directly
            self.inner.write_all(&self.buffer)?;
        }

        self.buffer.clear();
        Ok(())
    }

    /// Finishes the compression stream and returns the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        // Make decision if we haven't yet (for small files)
        if !self.decision_made {
            self.make_decision()?;
        }

        // Write any remaining compressed data
        if !self.compress_buffer.is_empty() {
            self.inner.write_all(&self.compress_buffer)?;
        }

        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for AdaptiveCompressor<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.decision_made {
            // Buffer data until we have enough for auto-detection
            let remaining = self.decider.sample_size().saturating_sub(self.buffer.len());

            if remaining > 0 {
                let to_buffer = buf.len().min(remaining);
                self.buffer.extend_from_slice(&buf[..to_buffer]);

                // If we still don't have enough, report buffered amount
                if self.buffer.len() < self.decider.sample_size() {
                    return Ok(to_buffer);
                }
            }

            // We have enough data, make the decision
            self.make_decision()?;

            // Write any remaining data from this call
            if remaining < buf.len() {
                let written = self.write(&buf[remaining..])?;
                return Ok(written + remaining);
            }

            return Ok(buf.len());
        }

        // Decision already made, write data accordingly
        if self.should_compress {
            // Compress this chunk and add to buffer
            let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), self.level);
            encoder.write_all(buf)?;
            let (compressed, _) = encoder.finish_into_inner()?;
            self.compress_buffer.extend_from_slice(&compressed);
            Ok(buf.len())
        } else {
            self.inner.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Write any pending compressed data
        if !self.compress_buffer.is_empty() {
            self.inner.write_all(&self.compress_buffer)?;
            self.compress_buffer.clear();
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_skip_list_includes_common_formats() {
        let decider = CompressionDecider::with_default_skip_list();

        // Images
        assert!(decider.skip_extensions().contains("jpg"));
        assert!(decider.skip_extensions().contains("jpeg"));
        assert!(decider.skip_extensions().contains("png"));
        assert!(decider.skip_extensions().contains("gif"));
        assert!(decider.skip_extensions().contains("webp"));
        assert!(decider.skip_extensions().contains("heic"));

        // Video
        assert!(decider.skip_extensions().contains("mp4"));
        assert!(decider.skip_extensions().contains("mkv"));
        assert!(decider.skip_extensions().contains("avi"));
        assert!(decider.skip_extensions().contains("mov"));
        assert!(decider.skip_extensions().contains("webm"));

        // Audio
        assert!(decider.skip_extensions().contains("mp3"));
        assert!(decider.skip_extensions().contains("flac"));
        assert!(decider.skip_extensions().contains("ogg"));
        assert!(decider.skip_extensions().contains("m4a"));
        assert!(decider.skip_extensions().contains("aac"));

        // Archives
        assert!(decider.skip_extensions().contains("zip"));
        assert!(decider.skip_extensions().contains("gz"));
        assert!(decider.skip_extensions().contains("bz2"));
        assert!(decider.skip_extensions().contains("xz"));
        assert!(decider.skip_extensions().contains("7z"));
        assert!(decider.skip_extensions().contains("rar"));
        assert!(decider.skip_extensions().contains("zst"));

        // Documents
        assert!(decider.skip_extensions().contains("pdf"));
    }

    #[test]
    fn should_compress_skips_known_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("video.mp4"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("archive.zip"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn should_compress_case_insensitive() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new("PHOTO.JPG"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("Video.MP4"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn should_compress_unknown_extension_returns_auto_detect() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new("data.xyz"), None),
            CompressionDecision::AutoDetect
        );
    }

    #[test]
    fn should_compress_with_content_makes_decision() {
        let decider = CompressionDecider::with_default_skip_list();

        // Text content should compress
        let text_content = b"Hello, world! This is some compressible text content.";
        assert_eq!(
            decider.should_compress(Path::new("file.txt"), Some(text_content)),
            CompressionDecision::Compress
        );
    }

    #[test]
    fn magic_byte_detection_jpeg() {
        let decider = CompressionDecider::with_default_skip_list();

        // JPEG magic bytes
        let jpeg_header = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&jpeg_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_png() {
        let decider = CompressionDecider::with_default_skip_list();

        // PNG magic bytes
        let png_header = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&png_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_gzip() {
        let decider = CompressionDecider::with_default_skip_list();

        // gzip magic bytes
        let gzip_header = [0x1f, 0x8b, 0x08, 0x00];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&gzip_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_zip() {
        let decider = CompressionDecider::with_default_skip_list();

        // ZIP magic bytes
        let zip_header = [b'P', b'K', 0x03, 0x04];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&zip_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_pdf() {
        let decider = CompressionDecider::with_default_skip_list();

        // PDF magic bytes
        let pdf_header = b"%PDF-1.5";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(pdf_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn parse_skip_compress_list() {
        let decider = CompressionDecider::from_skip_compress_list("txt/log/csv");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn parse_skip_compress_list_with_dots() {
        let decider = CompressionDecider::from_skip_compress_list(".txt/.log/.csv");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn parse_skip_compress_list_whitespace() {
        let decider = CompressionDecider::from_skip_compress_list("txt  log\tcsv");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn auto_detect_compressible_repetitive_data() {
        let decider = CompressionDecider::new();

        // Highly repetitive data should compress well
        let data = vec![b'a'; 4096];
        assert!(decider.auto_detect_compressible(&data).unwrap());
    }

    #[test]
    fn auto_detect_compressible_random_like_data() {
        let decider = CompressionDecider::new();

        // Pseudo-random data using a better mixing function (PCG-like)
        let mut state: u64 = 0x853c49e6748fea9b;
        let data: Vec<u8> = (0..4096)
            .map(|_| {
                // PCG-style PRNG for high-quality randomness
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
                let rot = (state >> 59) as u32;
                ((xorshifted >> rot) | (xorshifted << ((32u32.wrapping_sub(rot)) & 31))) as u8
            })
            .collect();

        // High-entropy data should not compress well (ratio >= threshold)
        assert!(!decider.auto_detect_compressible(&data).unwrap());
    }

    #[test]
    fn auto_detect_compressible_empty() {
        let decider = CompressionDecider::new();
        assert!(decider.auto_detect_compressible(&[]).unwrap());
    }

    #[test]
    fn extract_extension_simple() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("file.txt")),
            Some("txt".to_owned())
        );
    }

    #[test]
    fn extract_extension_compound() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("archive.tar.gz")),
            Some("tar.gz".to_owned())
        );
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("backup.tar.bz2")),
            Some("tar.bz2".to_owned())
        );
    }

    #[test]
    fn extract_extension_uppercase() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("FILE.TXT")),
            Some("txt".to_owned())
        );
    }

    #[test]
    fn extract_extension_no_extension() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("Makefile")),
            None
        );
    }

    #[test]
    fn add_remove_extension() {
        let mut decider = CompressionDecider::new();

        decider.add_skip_extension("xyz");
        assert!(decider.skip_extensions().contains("xyz"));

        decider.remove_skip_extension("xyz");
        assert!(!decider.skip_extensions().contains("xyz"));
    }

    #[test]
    fn clear_extensions() {
        let mut decider = CompressionDecider::with_default_skip_list();
        assert!(!decider.skip_extensions().is_empty());

        decider.clear_skip_extensions();
        assert!(decider.skip_extensions().is_empty());
    }

    #[test]
    fn set_compression_threshold() {
        let mut decider = CompressionDecider::new();
        decider.set_compression_threshold(0.85);
        assert!((decider.compression_threshold() - 0.85).abs() < f64::EPSILON);

        // Test clamping
        decider.set_compression_threshold(1.5);
        assert!((decider.compression_threshold() - 1.0).abs() < f64::EPSILON);

        decider.set_compression_threshold(-0.5);
        assert!((decider.compression_threshold() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn set_sample_size() {
        let mut decider = CompressionDecider::new();
        decider.set_sample_size(8192);
        assert_eq!(decider.sample_size(), 8192);

        // Test minimum
        decider.set_sample_size(10);
        assert_eq!(decider.sample_size(), 64); // Minimum is 64
    }

    #[test]
    fn toggle_magic_detection() {
        let mut decider = CompressionDecider::new();
        assert!(decider.use_magic_detection());

        decider.set_use_magic_detection(false);
        assert!(!decider.use_magic_detection());

        // With magic detection disabled, JPEG header should not trigger skip
        let jpeg_header = [0xff, 0xd8, 0xff, 0xe0];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&jpeg_header)),
            CompressionDecision::Compress
        );
    }

    #[test]
    fn file_category_compressible() {
        assert!(!FileCategory::Image.is_compressible());
        assert!(!FileCategory::Video.is_compressible());
        assert!(!FileCategory::Audio.is_compressible());
        assert!(!FileCategory::Archive.is_compressible());
        assert!(!FileCategory::Document.is_compressible());
        assert!(FileCategory::Text.is_compressible());
        assert!(FileCategory::Data.is_compressible());
        assert!(FileCategory::Executable.is_compressible());
        assert!(FileCategory::Unknown.is_compressible());
    }

    #[test]
    fn riff_container_detection() {
        let decider = CompressionDecider::with_default_skip_list();

        // AVI (RIFF....AVI )
        let avi_header = b"RIFF\x00\x00\x00\x00AVI ";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(avi_header)),
            CompressionDecision::Skip
        );

        // WAV (RIFF....WAVE)
        let wav_header = b"RIFF\x00\x00\x00\x00WAVE";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(wav_header)),
            CompressionDecision::Skip
        );

        // WEBP (RIFF....WEBP)
        let webp_header = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(webp_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_signature_matches() {
        let sig = MagicSignature::new(0, b"TEST", FileCategory::Unknown);
        assert!(sig.matches(b"TEST1234"));
        assert!(!sig.matches(b"NOPE1234"));
        assert!(!sig.matches(b"TES")); // Too short
    }

    #[test]
    fn magic_signature_with_offset() {
        let sig = MagicSignature::new(4, b"DATA", FileCategory::Unknown);
        assert!(sig.matches(b"XXXXDATAYYY"));
        assert!(!sig.matches(b"DATXXXXX"));
    }

    #[test]
    fn adaptive_compressor_basic() {
        let decider = CompressionDecider::new();
        let mut output = Vec::new();
        let mut compressor = AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Fast);

        // Write compressible data
        let data = vec![b'a'; 8192];
        compressor.write_all(&data).unwrap();

        let output = compressor.finish().unwrap();

        // Compressible data should result in smaller output
        assert!(output.len() < data.len());
    }

    #[test]
    fn adaptive_compressor_forced_decision() {
        let decider = CompressionDecider::new();
        let mut output = Vec::new();
        let mut compressor = AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Fast);

        // Force no compression
        compressor.set_decision(false);
        assert_eq!(compressor.compression_enabled(), Some(false));

        let data = b"test data";
        compressor.write_all(data).unwrap();
        let output = compressor.finish().unwrap();

        // Output should be identical to input (no compression)
        assert_eq!(&output[..], data);
    }

    #[test]
    fn adaptive_compressor_small_write() {
        let mut decider = CompressionDecider::new();
        decider.set_sample_size(100);

        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        // Write less than sample size
        compressor.write_all(b"small").unwrap();

        // Decision shouldn't be made yet
        assert_eq!(compressor.compression_enabled(), None);

        // Finish should still work
        let _ = compressor.finish().unwrap();
    }

    #[test]
    fn compression_decider_default() {
        let decider = CompressionDecider::default();
        assert!(!decider.skip_extensions().is_empty());
    }
}
