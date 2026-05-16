//! Fuzzy basis file matching for delta transfers.
//!
//! With `--fuzzy` enabled, the destination directory (and at level 2 the
//! `--compare-dest`/`--copy-dest`/`--link-dest` directories) is scanned for
//! similarly-named files that can serve as basis files for delta transfer.
//! Candidates are scored on extension, prefix, suffix, and size similarity;
//! the highest-scoring file above the minimum threshold wins.
//!
//! upstream: generator.c:1580-1700 `find_fuzzy_basis()`,
//! generator.c:1450-1530 `fuzzy_find()`.

mod scoring;
mod search;
mod trace;

pub use scoring::compute_similarity_score;
pub use trace::{trace_fuzzy_basis_selected, trace_fuzzy_distance, trace_fuzzy_size_mtime_match};

use std::path::PathBuf;

/// Minimum score for a candidate to be considered a fuzzy basis match.
const MIN_FUZZY_SCORE: u32 = 10;

/// Bonus when target and candidate share a file extension.
const EXTENSION_MATCH_BONUS: u32 = 50;

/// Points awarded per matching prefix character.
///
/// Naming conventions typically place distinguishing information at the
/// tail, so prefix matches are the strongest similarity signal.
const PREFIX_MATCH_POINTS: u32 = 10;

/// Points awarded per matching suffix character (before extension).
///
/// Weighted lower than prefix matches to avoid over-weighting accidental
/// tail collisions.
const SUFFIX_MATCH_POINTS: u32 = 8;

/// Bonus when candidate size is within 50% of the target size.
const SIZE_SIMILARITY_BONUS: u32 = 30;

/// Fuzzy level for a single `--fuzzy` flag; searches the destination directory.
pub const FUZZY_LEVEL_1: u8 = 1;

/// Fuzzy level for `-yy`; searches destination directory plus reference
/// directories (`--compare-dest`, `--copy-dest`, `--link-dest`).
///
/// upstream: options.c:2120 - when `fuzzy_basis > 1`, the value is set to
/// `basis_dir_cnt + 1` so the search iterates over the dest dir (index 0)
/// plus each reference directory.
pub const FUZZY_LEVEL_2: u8 = 2;

/// Result of fuzzy matching search.
///
/// Contains the path to the best matching file and its similarity score.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    /// Path to the matching basis file.
    pub path: PathBuf,
    /// Similarity score (higher is better).
    pub score: u32,
}

/// Fuzzy matcher for finding similar basis files.
///
/// upstream: generator.c:1580 `find_fuzzy_basis()`.
#[derive(Debug, Default)]
pub struct FuzzyMatcher {
    /// Fuzzy matching level (1 or 2).
    fuzzy_level: u8,
    /// Minimum score required for a match.
    min_score: u32,
    /// Additional directories to search (for level 2 fuzzy matching).
    pub(crate) fuzzy_basis_dirs: Vec<PathBuf>,
}

impl FuzzyMatcher {
    /// Creates a new level-1 fuzzy matcher with the default minimum score.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fuzzy_level: FUZZY_LEVEL_1,
            min_score: MIN_FUZZY_SCORE,
            fuzzy_basis_dirs: Vec::new(),
        }
    }

    /// Creates a new fuzzy matcher with the specified level.
    #[must_use]
    pub const fn with_level(level: u8) -> Self {
        Self {
            fuzzy_level: level,
            min_score: MIN_FUZZY_SCORE,
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

    /// Overrides the minimum score threshold; candidates below the threshold
    /// are discarded.
    pub const fn with_min_score(mut self, score: u32) -> Self {
        self.min_score = score;
        self
    }

    /// Returns the current fuzzy level.
    #[must_use]
    pub const fn fuzzy_level(&self) -> u8 {
        self.fuzzy_level
    }

    /// Returns the current minimum score threshold.
    #[must_use]
    pub const fn min_score(&self) -> u32 {
        self.min_score
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
        fn scoring_constants_reasonable() {
            // Pin the relative weights: extension > prefix > suffix; size
            // bonus is meaningful but never dominates name similarity.
            assert_eq!(EXTENSION_MATCH_BONUS, 50);
            assert_eq!(PREFIX_MATCH_POINTS, 10);
            assert_eq!(SUFFIX_MATCH_POINTS, 8);
            assert_eq!(SIZE_SIMILARITY_BONUS, 30);
            assert_eq!(MIN_FUZZY_SCORE, 10);
        }
    }
}
