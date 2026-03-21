//! Default skip-compress extension lists.
//!
//! These lists mirror upstream rsync's built-in set of file extensions that
//! typically don't benefit from compression during transfer.

use std::collections::HashSet;

use super::Suffix;

/// Known compound extensions checked before simple extension lookup.
///
/// These multi-part suffixes (e.g., `tar.gz`) must be matched against the full
/// filename to avoid false positives from the final extension alone.
pub const COMPOUND_EXTENSIONS: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"];

/// Image file extensions that are already compressed.
const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "gif", "webp", "heic", "heif", "avif", "tif", "tiff", "bmp",
    "ico", "svg", "svgz", "psd", "raw", "arw", "cr2", "nef", "orf", "sr2",
];

/// Video file extensions that are already compressed.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mkv", "avi", "mov", "wmv", "flv", "webm", "mpeg", "mpg", "vob", "ogv", "3gp",
    "3g2", "ts", "mts", "m2ts",
];

/// Audio file extensions that are already compressed.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wma", "wav", "aiff", "ape", "mka", "ac3",
    "dts",
];

/// Archive and compressed file extensions.
const ARCHIVE_EXTENSIONS: &[&str] = &[
    "zip", "gz", "gzip", "bz2", "bzip2", "xz", "lzma", "7z", "rar", "zst", "zstd", "lz4", "lzo",
    "z", "cab", "arj", "lzh", "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "tgz", "tbz", "tbz2",
    "txz",
];

/// Package format extensions (pre-compressed).
const PACKAGE_EXTENSIONS: &[&str] = &[
    "deb", "rpm", "apk", "jar", "war", "ear", "egg", "whl", "gem", "nupkg", "snap", "appx", "msix",
];

/// Document format extensions (often pre-compressed internally).
const DOCUMENT_EXTENSIONS: &[&str] = &[
    "pdf", "epub", "mobi", "azw", "azw3", "docx", "xlsx", "pptx", "odt", "ods", "odp",
];

/// Disk image extensions (often compressed or encrypted).
const DISK_IMAGE_EXTENSIONS: &[&str] =
    &["iso", "img", "dmg", "vhd", "vhdx", "vmdk", "qcow", "qcow2"];

/// All default extension groups in declaration order.
const ALL_GROUPS: &[&[&str]] = &[
    IMAGE_EXTENSIONS,
    VIDEO_EXTENSIONS,
    AUDIO_EXTENSIONS,
    ARCHIVE_EXTENSIONS,
    PACKAGE_EXTENSIONS,
    DOCUMENT_EXTENSIONS,
    DISK_IMAGE_EXTENSIONS,
];

/// Returns the complete default set of skip-compress extensions.
///
/// Each entry is a `Suffix` in canonical lowercase form. The list mirrors
/// upstream rsync's built-in skip-compress defaults.
#[must_use]
pub fn default_skip_extensions() -> HashSet<Suffix> {
    let total: usize = ALL_GROUPS.iter().map(|g| g.len()).sum();
    let mut set = HashSet::with_capacity(total);
    for group in ALL_GROUPS {
        for ext in *group {
            set.insert(Suffix::new(ext));
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_set_is_non_empty() {
        let exts = default_skip_extensions();
        assert!(exts.len() > 80, "expected at least 80 default extensions");
    }

    #[test]
    fn default_set_contains_common_formats() {
        let exts = default_skip_extensions();
        for expected in &["jpg", "mp4", "mp3", "zip", "pdf", "iso"] {
            assert!(exts.contains(*expected), "missing extension: {expected}");
        }
    }

    #[test]
    fn compound_extensions_included() {
        let exts = default_skip_extensions();
        assert!(exts.contains("tar.gz"));
        assert!(exts.contains("tar.bz2"));
    }
}
