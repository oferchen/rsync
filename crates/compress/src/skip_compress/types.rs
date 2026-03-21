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
