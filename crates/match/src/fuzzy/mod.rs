//! Fuzzy basis file matching for delta transfers.
//!
//! When `--fuzzy` is enabled, this module searches the destination directory
//! for files with similar names that can serve as basis files for delta
//! transfer, reducing data transfer even when no exact match exists.
//!
//! # Upstream Reference
//!
//! This implementation mirrors upstream rsync's fuzzy matching algorithm from:
//! - `generator.c:1580-1700` - `find_fuzzy_basis()` function
//! - `generator.c:1450-1530` - `fuzzy_find()` scoring algorithm
//!
//! # Algorithm
//!
//! The fuzzy matching algorithm compares candidate filenames against the target
//! filename using a multi-factor scoring system:
//!
//! 1. **Extension matching** - Files with the same extension score higher
//! 2. **Prefix matching** - Longer common prefixes increase the score
//! 3. **Suffix matching** - Common suffixes (before extension) add points
//! 4. **Size similarity** - Files with similar sizes get a bonus
//!
//! The highest-scoring file above the minimum threshold is selected as the
//! fuzzy basis file.
//!
//! # Fuzzy Levels
//!
//! - **Level 1** (`--fuzzy`): Search only in the destination directory
//! - **Level 2** (`-yy`): Also search in `--compare-dest`, `--copy-dest`, and
//!   `--link-dest` directories
//!
//! # Examples
//!
//! ```
//! use matching::FuzzyMatcher;
//! use std::ffi::OsStr;
//! use std::path::Path;
//!
//! # use tempfile::TempDir;
//! # use std::fs;
//! # let temp = TempDir::new().unwrap();
//! # fs::write(temp.path().join("report_2023.csv"), "old").unwrap();
//!
//! let matcher = FuzzyMatcher::new();
//! let result = matcher.find_fuzzy_basis(
//!     OsStr::new("report_2024.csv"),
//!     temp.path(),
//!     1000, // target size
//! );
//!
//! // Will find "report_2023.csv" as a fuzzy match
//! assert!(result.is_some());
//! ```

mod scoring;
mod search;

pub use scoring::compute_similarity_score;

use std::path::PathBuf;

/// Score threshold for considering a file as a fuzzy match.
/// Files must score above this to be considered as basis candidates.
///
/// Upstream rsync uses a similar threshold to filter out poor matches.
const MIN_FUZZY_SCORE: u32 = 10;

/// Bonus score for matching file extension.
///
/// Extension matches are weighted heavily because they indicate
/// the same file type, which correlates with similar content structure.
const EXTENSION_MATCH_BONUS: u32 = 50;

/// Points per character of matching prefix.
///
/// Prefix matches are the strongest indicator of file similarity,
/// as filesystem naming conventions typically place distinguishing
/// information at the end (versions, dates, etc.).
const PREFIX_MATCH_POINTS: u32 = 10;

/// Points per character of matching suffix (before extension).
///
/// Suffix matches contribute to similarity but are weighted lower
/// than prefix matches to avoid over-weighting accidental matches.
const SUFFIX_MATCH_POINTS: u32 = 8;

/// Bonus for similar file size (within 50% of target).
///
/// Size similarity indicates the files likely contain similar amounts
/// of data, making delta transfers more efficient.
const SIZE_SIMILARITY_BONUS: u32 = 30;

/// Fuzzy level for single --fuzzy flag.
/// Searches only in the destination directory.
pub const FUZZY_LEVEL_1: u8 = 1;

/// Fuzzy level for -yy (double --fuzzy).
/// Searches in destination directory AND reference directories
/// (`--compare-dest`, `--copy-dest`, `--link-dest`).
///
/// Upstream `options.c:2120`: when `fuzzy_basis > 1`, the value is set
/// to `basis_dir_cnt + 1`, making the fuzzy search iterate over the
/// dest dir (index 0) plus each reference directory.
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
/// # Upstream Reference
///
/// Mirrors the behavior of `find_fuzzy_basis()` in `generator.c:1580`.
///
/// # Examples
///
/// ```
/// use matching::FuzzyMatcher;
/// use std::path::PathBuf;
///
/// // Level 1 fuzzy: search only destination directory
/// let matcher1 = FuzzyMatcher::new();
///
/// // Level 2 fuzzy: also search reference directories
/// let ref_dirs = vec![PathBuf::from("/backup/old")];
/// let matcher2 = FuzzyMatcher::with_level(2)
///     .with_fuzzy_basis_dirs(ref_dirs);
/// ```
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
    /// Creates a new fuzzy matcher with default settings (level 1).
    ///
    /// # Defaults
    ///
    /// - Fuzzy level: 1 (search only destination directory)
    /// - Minimum score: 10 (filters out very poor matches)
    /// - No additional fuzzy basis directories
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fuzzy_level: FUZZY_LEVEL_1,
            min_score: MIN_FUZZY_SCORE,
            fuzzy_basis_dirs: Vec::new(),
        }
    }

    /// Creates a new fuzzy matcher with the specified level.
    ///
    /// # Arguments
    ///
    /// * `level` - Fuzzy matching level (1 or 2)
    ///
    /// # Examples
    ///
    /// ```
    /// use matching::FuzzyMatcher;
    ///
    /// let matcher = FuzzyMatcher::with_level(2); // -yy mode
    /// ```
    #[must_use]
    pub const fn with_level(level: u8) -> Self {
        Self {
            fuzzy_level: level,
            min_score: MIN_FUZZY_SCORE,
            fuzzy_basis_dirs: Vec::new(),
        }
    }

    /// Sets additional fuzzy basis directories (for level 2 fuzzy matching).
    ///
    /// These directories are searched in addition to the destination directory
    /// when fuzzy level is 2 (`-yy`). Corresponds to `--compare-dest`,
    /// `--copy-dest`, and `--link-dest` directories in upstream rsync.
    ///
    /// # Examples
    ///
    /// ```
    /// use matching::FuzzyMatcher;
    /// use std::path::PathBuf;
    ///
    /// let dirs = vec![PathBuf::from("/backup/2023"), PathBuf::from("/backup/2022")];
    /// let matcher = FuzzyMatcher::with_level(2)
    ///     .with_fuzzy_basis_dirs(dirs);
    /// ```
    pub fn with_fuzzy_basis_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.fuzzy_basis_dirs = dirs;
        self
    }

    /// Sets the minimum score threshold.
    ///
    /// Files with scores below this threshold will not be considered as
    /// fuzzy matches. Lower thresholds allow more distant matches but may
    /// result in less efficient delta transfers.
    ///
    /// # Examples
    ///
    /// ```
    /// use matching::FuzzyMatcher;
    ///
    /// // Require higher similarity
    /// let strict_matcher = FuzzyMatcher::new().with_min_score(100);
    ///
    /// // Allow looser matches
    /// let relaxed_matcher = FuzzyMatcher::new().with_min_score(5);
    /// ```
    pub const fn with_min_score(mut self, score: u32) -> Self {
        self.min_score = score;
        self
    }

    /// Gets the current fuzzy level.
    #[must_use]
    pub const fn fuzzy_level(&self) -> u8 {
        self.fuzzy_level
    }

    /// Gets the current minimum score threshold.
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
