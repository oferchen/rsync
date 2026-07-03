//! Default skip-compress suffix list.
//!
//! This list mirrors upstream rsync's built-in `DEFAULT_DONT_COMPRESS` set of
//! file suffixes that typically don't benefit from compression during transfer.
//!
//! upstream: `default-dont-compress.h` defines `DEFAULT_DONT_COMPRESS`, which
//! `token.c:init_set_compression()` loads into the suffix tree via
//! `loadparm.c:lp_dont_compress()`. Each upstream entry is a `*.suffix` glob
//! whose match key is the substring after the final `.` (upstream matches with
//! `strrchr(fname, '.')`, i.e. the last suffix only - there is no compound
//! `.tar.gz` handling).

use std::collections::HashSet;

use super::Suffix;

/// Upstream default suffixes that skip compression.
///
/// This is the exact set from upstream `default-dont-compress.h`
/// (`DEFAULT_DONT_COMPRESS`), stored without the leading `*.` and lowercased to
/// match upstream's case-insensitive suffix comparison.
const DEFAULT_DONT_COMPRESS: &[&str] = &[
    "3g2", "3gp", "7z", "aac", "ace", "apk", "avi", "bz2", "deb", "dmg", "ear", "f4v", "flac",
    "flv", "gpg", "gz", "iso", "jar", "jpeg", "jpg", "lrz", "lz", "lz4", "lzma", "lzo", "m1a",
    "m1v", "m2a", "m2ts", "m2v", "m4a", "m4b", "m4p", "m4r", "m4v", "mka", "mkv", "mov", "mp1",
    "mp2", "mp3", "mp4", "mpa", "mpeg", "mpg", "mpv", "mts", "odb", "odf", "odg", "odi", "odm",
    "odp", "ods", "odt", "oga", "ogg", "ogm", "ogv", "ogx", "opus", "otg", "oth", "otp", "ots",
    "ott", "oxt", "png", "qt", "rar", "rpm", "rz", "rzip", "spx", "squashfs", "sxc", "sxd", "sxg",
    "sxm", "sxw", "sz", "tbz", "tbz2", "tgz", "tlz", "ts", "txz", "tzo", "vob", "war", "webm",
    "webp", "xz", "z", "zip", "zst",
];

/// Returns the complete default set of skip-compress suffixes.
///
/// Each entry is a `Suffix` in canonical lowercase form. The list is the exact
/// upstream `DEFAULT_DONT_COMPRESS` set.
#[must_use]
pub fn default_skip_extensions() -> HashSet<Suffix> {
    let mut set = HashSet::with_capacity(DEFAULT_DONT_COMPRESS.len());
    for ext in DEFAULT_DONT_COMPRESS {
        set.insert(Suffix::new(ext));
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default set must contain exactly the upstream `DEFAULT_DONT_COMPRESS`
    /// suffixes. A count drift here means the list diverged from upstream, which
    /// changes which files get compressed on the wire vs upstream rsync.
    #[test]
    fn default_set_matches_upstream_count() {
        let exts = default_skip_extensions();
        assert_eq!(
            exts.len(),
            DEFAULT_DONT_COMPRESS.len(),
            "default skip set must equal upstream DEFAULT_DONT_COMPRESS ({} suffixes)",
            DEFAULT_DONT_COMPRESS.len(),
        );
    }

    /// Every upstream suffix must be present. Guards against silently dropping a
    /// suffix upstream skips (which would compress an already-compressed file).
    #[test]
    fn default_set_contains_every_upstream_suffix() {
        let exts = default_skip_extensions();
        for expected in DEFAULT_DONT_COMPRESS {
            assert!(
                exts.contains(*expected),
                "missing upstream suffix: {expected}"
            );
        }
    }

    /// Suffixes that upstream does NOT list must not be skipped. oc-rsync
    /// previously invented entries (e.g. `pdf`, `docx`, `heic`, `wav`) that
    /// upstream compresses; skipping them diverges from upstream on the wire.
    #[test]
    fn default_set_excludes_non_upstream_suffixes() {
        let exts = default_skip_extensions();
        for unexpected in &[
            "pdf", "docx", "xlsx", "pptx", "epub", "heic", "heif", "avif", "tiff", "bmp", "wav",
            "wma", "aiff", "gzip", "bzip2", "img", "vhd", "vmdk", "qcow2", "cab", "whl", "gem",
        ] {
            assert!(
                !exts.contains(*unexpected),
                "suffix {unexpected} is not in upstream DEFAULT_DONT_COMPRESS and must not be skipped",
            );
        }
    }
}
