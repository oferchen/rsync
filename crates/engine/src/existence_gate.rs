//! The `--existing` / `--ignore-existing` gate shared by the local-copy
//! executor and the network receiver.
//!
//! upstream: generator.c:1757-1806 recv_generator() - both tests run on the
//! destination `lstat` taken before any obstacle removal, for every file type:
//!
//! - `ignore_non_existing > 0 && statret == -1 && stat_errno == ENOENT` skips an
//!   absent destination ("not creating new file|directory"). A skipped directory
//!   becomes `skip_dir`, so its whole subtree is skipped with it.
//! - `ignore_existing > 0 && statret == 0 && (!is_dir || stype != FT_DIR)` skips
//!   an existing destination ("%s exists"), except a directory arriving over a
//!   directory, which is still merged into.

use std::fs;
use std::io;

/// Why the existence gate skips an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistenceSkip {
    /// `--existing`: the destination is absent, so nothing new is created.
    NotCreatingNew,
    /// `--ignore-existing`: the destination is present and is left untouched.
    Exists,
}

/// The destination as the generator's pre-transfer `lstat` sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationEntry {
    /// The `lstat` failed with `ENOENT`.
    Missing,
    /// The destination is a directory (or, under `--keep-dirlinks`, a symlink
    /// resolving to one).
    Directory,
    /// The destination is any other file type.
    Other,
    /// The `lstat` failed with an errno other than `ENOENT`; neither gate
    /// applies and the caller reports the failure.
    Unknown,
}

impl DestinationEntry {
    /// Classifies the result of a destination `lstat`.
    #[must_use]
    pub fn from_lstat(result: &io::Result<fs::Metadata>) -> Self {
        match result {
            Ok(meta) => Self::from_metadata(meta),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::Missing,
            Err(_) => Self::Unknown,
        }
    }

    /// Classifies an existing destination's metadata.
    #[must_use]
    pub fn from_metadata(meta: &fs::Metadata) -> Self {
        if meta.file_type().is_dir() {
            Self::Directory
        } else {
            Self::Other
        }
    }

    /// Classifies an optional destination, where `None` means absent.
    #[must_use]
    pub fn from_optional(meta: Option<&fs::Metadata>) -> Self {
        meta.map_or(Self::Missing, Self::from_metadata)
    }
}

/// Decides whether `--existing` (`existing_only`) or `--ignore-existing`
/// skips an entry whose source is a directory iff `source_is_dir`.
#[must_use]
pub const fn existence_skip(
    existing_only: bool,
    ignore_existing: bool,
    source_is_dir: bool,
    destination: DestinationEntry,
) -> Option<ExistenceSkip> {
    match destination {
        DestinationEntry::Missing if existing_only => Some(ExistenceSkip::NotCreatingNew),
        DestinationEntry::Directory if ignore_existing && !source_is_dir => {
            Some(ExistenceSkip::Exists)
        }
        DestinationEntry::Other if ignore_existing => Some(ExistenceSkip::Exists),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{DestinationEntry, ExistenceSkip, existence_skip};

    const ALL: [DestinationEntry; 4] = [
        DestinationEntry::Missing,
        DestinationEntry::Directory,
        DestinationEntry::Other,
        DestinationEntry::Unknown,
    ];

    /// Neither option set: the gate never skips anything.
    #[test]
    fn no_option_never_skips() {
        for dest in ALL {
            assert_eq!(existence_skip(false, false, false, dest), None);
            assert_eq!(existence_skip(false, false, true, dest), None);
        }
    }

    /// upstream: generator.c:1757 - `--existing` skips only an absent
    /// destination, for files and directories alike.
    #[test]
    fn existing_only_skips_only_absent_destinations() {
        for source_is_dir in [false, true] {
            assert_eq!(
                existence_skip(true, false, source_is_dir, DestinationEntry::Missing),
                Some(ExistenceSkip::NotCreatingNew)
            );
            for dest in [
                DestinationEntry::Directory,
                DestinationEntry::Other,
                DestinationEntry::Unknown,
            ] {
                assert_eq!(existence_skip(true, false, source_is_dir, dest), None);
            }
        }
    }

    /// upstream: generator.c:1784-1785 - `--ignore-existing` skips any present
    /// destination unless a directory is merged into a directory.
    #[test]
    fn ignore_existing_skips_present_destinations_except_dir_into_dir() {
        assert_eq!(
            existence_skip(false, true, true, DestinationEntry::Directory),
            None
        );
        assert_eq!(
            existence_skip(false, true, true, DestinationEntry::Other),
            Some(ExistenceSkip::Exists)
        );
        assert_eq!(
            existence_skip(false, true, false, DestinationEntry::Directory),
            Some(ExistenceSkip::Exists)
        );
        assert_eq!(
            existence_skip(false, true, false, DestinationEntry::Other),
            Some(ExistenceSkip::Exists)
        );
        for source_is_dir in [false, true] {
            assert_eq!(
                existence_skip(false, true, source_is_dir, DestinationEntry::Missing),
                None
            );
        }
    }

    /// A non-ENOENT stat failure is neither absent nor present.
    #[test]
    fn unknown_destination_is_never_skipped() {
        assert_eq!(
            existence_skip(true, true, false, DestinationEntry::Unknown),
            None
        );
    }
}
