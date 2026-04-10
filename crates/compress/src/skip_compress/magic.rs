//! Magic byte signatures for compressed file format detection.

use super::FileCategory;

/// Magic byte signature for detecting compressed file formats.
///
/// Each entry contains the byte offset and the expected bytes at that offset.
#[derive(Clone, Debug)]
pub struct MagicSignature {
    /// Offset from the start of the file.
    pub offset: usize,
    /// Expected bytes at the offset.
    pub bytes: &'static [u8],
    /// Category this signature identifies.
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
