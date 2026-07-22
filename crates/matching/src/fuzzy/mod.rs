//! Fuzzy basis file matching for delta transfers.
//!
//! With `--fuzzy` enabled, the destination directory (and at level 2 the
//! `--compare-dest`/`--copy-dest`/`--link-dest` directories) is scanned for a
//! similarly-named file to serve as the delta basis. Candidate selection is a
//! faithful port of upstream `generator.c:find_fuzzy()`:
//!
//! 1. An exact size + mtime match returns immediately (the "size/modtime"
//!    fast-path). upstream: generator.c:842-866.
//! 2. Otherwise the candidate with the lowest Levenshtein-style
//!    [`distance::fuzzy_distance`] (name plus 10x-weighted suffix) wins, provided
//!    it is within [`MAX_FUZZY_DISTANCE`]. upstream: generator.c:868-908.
//!
//! Upstream additionally skips candidates whose source-flist entry carries
//! `FLAG_FILE_SENT` (a file already generated in this same run must not become a
//! future fuzzy basis; generator.c:855,883,1879). oc scans the live destination
//! filesystem rather than a persistent per-directory flist and has no runtime
//! sent-flag reachable from this pure algorithm crate, so that screening is not
//! reproduced here. The reconstructed file always stays correct (the whole-file
//! checksum verifies it), but the selected basis - and therefore the delta the
//! sender emits - can differ from upstream when a just-updated sibling is the
//! closest name. Reproducing it faithfully needs the receiver to thread a
//! per-run "already generated" set down into candidate collection.
//!
//! upstream: generator.c:831 `find_fuzzy()`, util1.c:1588 `fuzzy_distance()`.

mod distance;
mod search;
mod trace;

pub use trace::{trace_fuzzy_basis_selected, trace_fuzzy_distance, trace_fuzzy_size_mtime_match};

use std::path::PathBuf;

/// Highest accepted fuzzy distance; a candidate above this is ignored.
///
/// upstream: generator.c:835 - `lowest_dist = 25 << 16` seeds the scan, so a
/// candidate qualifies only when its distance is `<= 25` Levenshtein units.
const MAX_FUZZY_DISTANCE: u32 = 25 << 16;

/// Fuzzy level for a single `--fuzzy` flag; searches the destination directory.
pub const FUZZY_LEVEL_1: u8 = 1;

/// Fuzzy level for `-yy`; searches destination directory plus reference
/// directories (`--compare-dest`, `--copy-dest`, `--link-dest`).
///
/// upstream: options.c:2083 - when `fuzzy_basis > 1`, the value is set to
/// `basis_dir_cnt + 1` so the search iterates over the dest dir (index 0)
/// plus each reference directory.
pub const FUZZY_LEVEL_2: u8 = 2;

/// Result of fuzzy matching search.
///
/// Contains the path to the best matching file and its edit distance.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    /// Path to the matching basis file.
    pub path: PathBuf,
    /// Levenshtein-style distance to the target name (lower is closer). Zero
    /// for a size/modtime fast-path hit. upstream: generator.c `lowest_dist`.
    pub distance: u32,
}

/// Fuzzy matcher for finding similar basis files.
///
/// upstream: generator.c:831 `find_fuzzy()`.
#[derive(Debug, Default)]
pub struct FuzzyMatcher {
    /// Fuzzy matching level (1 or 2).
    fuzzy_level: u8,
    /// Highest accepted distance; candidates above it are discarded.
    max_distance: u32,
    /// Additional directories to search (for level 2 fuzzy matching).
    pub(crate) fuzzy_basis_dirs: Vec<PathBuf>,
}

impl FuzzyMatcher {
    /// Creates a new level-1 fuzzy matcher with the default distance cap.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fuzzy_level: FUZZY_LEVEL_1,
            max_distance: MAX_FUZZY_DISTANCE,
            fuzzy_basis_dirs: Vec::new(),
        }
    }

    /// Creates a new fuzzy matcher with the specified level.
    #[must_use]
    pub const fn with_level(level: u8) -> Self {
        Self {
            fuzzy_level: level,
            max_distance: MAX_FUZZY_DISTANCE,
            fuzzy_basis_dirs: Vec::new(),
        }
    }

    /// Sets additional fuzzy basis directories searched at level 2.
    ///
    /// Corresponds to `--compare-dest`, `--copy-dest`, and `--link-dest`
    /// directories in upstream rsync.
    pub fn with_fuzzy_basis_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.fuzzy_basis_dirs = dirs;
        self
    }

    /// Overrides the maximum accepted distance; candidates above it are
    /// discarded.
    pub const fn with_max_distance(mut self, distance: u32) -> Self {
        self.max_distance = distance;
        self
    }

    /// Returns the current fuzzy level.
    #[must_use]
    pub const fn fuzzy_level(&self) -> u8 {
        self.fuzzy_level
    }

    /// Returns the current maximum accepted distance.
    #[must_use]
    pub const fn max_distance(&self) -> u32 {
        self.max_distance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod constants_tests {
        use super::*;

        #[test]
        fn fuzzy_level_constants() {
            assert_eq!(FUZZY_LEVEL_1, 1);
            assert_eq!(FUZZY_LEVEL_2, 2);
        }

        #[test]
        fn max_distance_matches_upstream_seed() {
            // upstream: generator.c:835 - lowest_dist starts at 25 << 16.
            assert_eq!(MAX_FUZZY_DISTANCE, 25 << 16);
            assert_eq!(FuzzyMatcher::new().max_distance(), 25 << 16);
        }
    }
}
