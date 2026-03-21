use std::fmt;
use std::hash::{Hash, Hasher};

/// Decision about whether to compress a file during transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CompressionDecision {
    /// The file should be compressed during transfer.
    Compress,
    /// The file should be transferred without compression.
    Skip,
    /// Compression decision should be made by sampling the file content.
    ///
    /// Returned when the file extension is not in any known list
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

/// A normalized file suffix used for compression skip-list lookups.
///
/// This value object ensures suffixes are always stored in a canonical form:
/// lowercase, without leading dots, and trimmed of whitespace. Using a dedicated
/// type instead of raw strings prevents mismatches from inconsistent
/// normalization and makes the intent of suffix-based operations explicit.
///
/// # Examples
///
/// ```
/// use compress::skip_compress::Suffix;
///
/// let s = Suffix::new(".JPG");
/// assert_eq!(s.as_str(), "jpg");
///
/// // Compound extensions are preserved
/// let compound = Suffix::new("tar.gz");
/// assert_eq!(compound.as_str(), "tar.gz");
/// ```
#[derive(Clone, Debug, Eq)]
pub struct Suffix(String);

impl Suffix {
    /// Creates a new suffix by normalizing the input.
    ///
    /// Leading dots are stripped, whitespace is trimmed, and the result is
    /// lowercased. Returns `None` if the normalized result is empty.
    #[must_use]
    pub fn new(raw: &str) -> Self {
        let normalized = raw.trim().trim_start_matches('.').to_ascii_lowercase();
        Self(normalized)
    }

    /// Returns the normalized suffix string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` if the normalized suffix is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl PartialEq for Suffix {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Hash for Suffix {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq<str> for Suffix {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl fmt::Display for Suffix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for Suffix {
    fn borrow(&self) -> &str {
        &self.0
    }
}
