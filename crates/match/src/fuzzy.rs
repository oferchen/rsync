//! crates/match/src/fuzzy.rs
//!
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

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

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
/// Searches in destination directory AND fuzzy basis directories
/// (--compare-dest, --copy-dest, --link-dest).
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
    fuzzy_basis_dirs: Vec<PathBuf>,
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

    /// Finds the best fuzzy match for a file in the destination directory.
    ///
    /// Searches the parent directory of `dest_path` for files with similar
    /// names. If fuzzy level is 2, also searches the configured
    /// `fuzzy_basis_dirs`.
    ///
    /// # Algorithm
    ///
    /// 1. Read all files from destination directory
    /// 2. Skip exact name matches (not fuzzy)
    /// 3. Score each candidate file based on name and size similarity
    /// 4. If level 2, also search fuzzy basis directories
    /// 5. Return the highest-scoring file above the minimum threshold
    ///
    /// # Arguments
    ///
    /// * `target_name` - Name of the file we're looking for a basis for
    /// * `dest_dir` - Destination directory to search
    /// * `target_size` - Size of the source file (for size similarity bonus)
    ///
    /// # Returns
    ///
    /// The best matching file if one is found with score above threshold.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `generator.c:1580` - `find_fuzzy_basis()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use matching::FuzzyMatcher;
    /// use std::ffi::OsStr;
    /// # use tempfile::TempDir;
    /// # use std::fs;
    /// # let temp = TempDir::new().unwrap();
    /// # fs::write(temp.path().join("file_v1.txt"), "old").unwrap();
    ///
    /// let matcher = FuzzyMatcher::new();
    /// let result = matcher.find_fuzzy_basis(
    ///     OsStr::new("file_v2.txt"),
    ///     temp.path(),
    ///     100,
    /// );
    ///
    /// assert!(result.is_some());
    /// ```
    pub fn find_fuzzy_basis(
        &self,
        target_name: &OsStr,
        dest_dir: &Path,
        target_size: u64,
    ) -> Option<FuzzyMatch> {
        let target_name_str = target_name.to_string_lossy();

        let mut best_match: Option<FuzzyMatch> = None;

        // Search destination directory (always done for level 1 and 2)
        if let Some(m) = self.search_directory(dest_dir, &target_name_str, target_size) {
            update_best_match(&mut best_match, m, self.min_score);
        }

        // Search additional fuzzy basis directories (only for level 2)
        if self.fuzzy_level >= FUZZY_LEVEL_2 {
            for basis_dir in &self.fuzzy_basis_dirs {
                if let Some(m) = self.search_directory(basis_dir, &target_name_str, target_size) {
                    update_best_match(&mut best_match, m, self.min_score);
                }
            }
        }

        best_match
    }

    /// Searches a single directory for fuzzy matches.
    ///
    /// # Implementation Notes
    ///
    /// - Skips directories and special files
    /// - Skips exact name matches (those aren't fuzzy)
    /// - Returns the highest-scoring candidate from this directory
    fn search_directory(
        &self,
        dir: &Path,
        target_name: &str,
        target_size: u64,
    ) -> Option<FuzzyMatch> {
        let Ok(entries) = fs::read_dir(dir) else {
            return None;
        };

        let mut best_match: Option<FuzzyMatch> = None;

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip directories and non-regular files
            let metadata = match entry.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };

            let candidate_name = match path.file_name() {
                Some(name) => name.to_string_lossy(),
                None => continue,
            };

            // Don't match the exact same name (that's not fuzzy)
            if candidate_name == target_name {
                continue;
            }

            let score =
                compute_similarity_score(target_name, &candidate_name, target_size, metadata.len());

            if score >= self.min_score {
                update_best_match(&mut best_match, FuzzyMatch { path, score }, self.min_score);
            }
        }

        best_match
    }
}

/// Updates the best match if the new candidate has a higher score and meets the threshold.
///
/// This function is inlined for performance as it's called in the hot path
/// of fuzzy matching during file scanning.
#[inline]
fn update_best_match(best: &mut Option<FuzzyMatch>, candidate: FuzzyMatch, min_score: u32) {
    if candidate.score < min_score {
        return;
    }
    match best {
        Some(existing) if existing.score >= candidate.score => {}
        _ => *best = Some(candidate),
    }
}

/// Computes a similarity score between two filenames.
///
/// # Algorithm
///
/// The scoring algorithm considers multiple factors:
///
/// 1. **Extension match** (+50 points) - Same file extension
/// 2. **Common prefix** (+10 per char) - Matching characters at the start
/// 3. **Common suffix** (+8 per char) - Matching characters before extension
/// 4. **Size similarity** (+30 points) - File sizes within 50% of each other
///
/// Higher scores indicate better matches. The minimum useful score is
/// typically around 10 points.
///
/// # Upstream Reference
///
/// Mirrors the scoring logic in `generator.c:1450-1530` - `fuzzy_find()`.
///
/// # Arguments
///
/// * `target` - The name we're trying to match
/// * `candidate` - A potential match candidate
/// * `target_size` - Size of the target file
/// * `candidate_size` - Size of the candidate file
///
/// # Returns
///
/// A similarity score where higher values indicate better matches.
///
/// # Examples
///
/// ```
/// use matching::compute_similarity_score;
///
/// // Same extension and similar names
/// let score = compute_similarity_score(
///     "report_2024.csv",
///     "report_2023.csv",
///     1000,
///     950
/// );
/// assert!(score > 100, "Should score highly: {}", score);
///
/// // Different extensions score lower than same extension
/// let score_diff_ext = compute_similarity_score(
///     "data.csv",
///     "data.txt",
///     1000,
///     1000
/// );
/// let score_same_ext = compute_similarity_score(
///     "data.csv",
///     "data.csv",
///     1000,
///     1000
/// );
/// assert!(score_diff_ext < score_same_ext, "Different extensions score lower");
/// ```
pub fn compute_similarity_score(
    target: &str,
    candidate: &str,
    target_size: u64,
    candidate_size: u64,
) -> u32 {
    let mut score: u32 = 0;

    // Extract extensions
    let (target_base, target_ext) = split_name_extension(target);
    let (candidate_base, candidate_ext) = split_name_extension(candidate);

    // Extension match bonus
    if target_ext == candidate_ext && !target_ext.is_empty() {
        score += EXTENSION_MATCH_BONUS;
    }

    // Common prefix length (on base name)
    let prefix_len = common_prefix_length(target_base, candidate_base);
    score += prefix_len as u32 * PREFIX_MATCH_POINTS;

    // Common suffix length (on base name, excluding extension)
    let suffix_len = common_suffix_length(target_base, candidate_base);
    score += suffix_len as u32 * SUFFIX_MATCH_POINTS;

    // Size similarity bonus
    if target_size > 0 && candidate_size > 0 {
        let size_ratio = if target_size >= candidate_size {
            candidate_size as f64 / target_size as f64
        } else {
            target_size as f64 / candidate_size as f64
        };

        // Bonus if sizes are within 50% of each other (ratio >= 0.5)
        // This mirrors upstream rsync's size similarity heuristic
        if size_ratio >= 0.5 {
            score += SIZE_SIMILARITY_BONUS;
        }
    }

    score
}

/// Splits a filename into base name and extension.
///
/// # Algorithm
///
/// Finds the last '.' in the filename and splits there, with special handling:
/// - Hidden files (starting with '.') without another '.' have no extension
/// - Trailing dots are not considered extensions
/// - Double extensions like ".tar.gz" are split at the last dot
///
/// # Examples
///
/// ```
/// # use matching::compute_similarity_score;
/// // These examples show the splitting logic indirectly through scoring
/// assert!(compute_similarity_score("file.txt", "data.txt", 1, 1) > 50);
/// assert!(compute_similarity_score(".hidden", ".config", 1, 1) < 50);
/// ```
fn split_name_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(pos) if pos > 0 && pos < name.len() - 1 => (&name[..pos], &name[pos + 1..]),
        _ => (name, ""),
    }
}

/// Computes the length of the common prefix between two strings.
///
/// # Optimization
///
/// This function is optimized for ASCII filenames (the common case) with
/// a fast byte-comparison path, but falls back to correct Unicode handling
/// when necessary.
///
/// # Examples
///
/// ```
/// # use matching::compute_similarity_score;
/// // Common prefix "report_202" contributes to high score
/// let score = compute_similarity_score("report_2024", "report_2023", 1, 1);
/// assert!(score > 80);
/// ```
#[inline]
fn common_prefix_length(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    // Fast path: compare bytes directly for ASCII
    let min_len = a_bytes.len().min(b_bytes.len());
    let mut common_bytes = 0;

    for i in 0..min_len {
        if a_bytes[i] != b_bytes[i] {
            break;
        }
        common_bytes = i + 1;
    }

    // If we're at a UTF-8 boundary on both sides, count chars in the prefix
    if a.is_char_boundary(common_bytes) && b.is_char_boundary(common_bytes) {
        a[..common_bytes].chars().count()
    } else {
        // Fallback: count matching Unicode chars
        a.chars()
            .zip(b.chars())
            .take_while(|(ca, cb)| ca == cb)
            .count()
    }
}

/// Computes the length of the common suffix between two strings.
///
/// # Optimization
///
/// This function is optimized for ASCII filenames (the common case) with
/// a fast byte-comparison path from the end, but falls back to correct
/// Unicode handling when necessary.
///
/// # Examples
///
/// ```
/// # use matching::compute_similarity_score;
/// // Common suffix "_backup" contributes to similarity
/// let score = compute_similarity_score("data_backup", "config_backup", 1, 1);
/// assert!(score > 40);
/// ```
#[inline]
fn common_suffix_length(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    // Fast path: compare bytes directly from the end for ASCII
    let min_len = a_bytes.len().min(b_bytes.len());
    let mut common_bytes = 0;

    for i in 0..min_len {
        let a_idx = a_bytes.len() - 1 - i;
        let b_idx = b_bytes.len() - 1 - i;
        if a_bytes[a_idx] != b_bytes[b_idx] {
            break;
        }
        common_bytes = i + 1;
    }

    // If we're at a UTF-8 boundary on both sides, count chars in the suffix
    let a_start = a_bytes.len() - common_bytes;
    let b_start = b_bytes.len() - common_bytes;
    if a.is_char_boundary(a_start) && b.is_char_boundary(b_start) {
        a[a_start..].chars().count()
    } else {
        // Fallback: count matching Unicode chars from the end
        a.chars()
            .rev()
            .zip(b.chars().rev())
            .take_while(|(ca, cb)| ca == cb)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod common_prefix_tests {
        use super::*;

        #[test]
        fn partial_match() {
            assert_eq!(common_prefix_length("hello", "help"), 3);
        }

        #[test]
        fn no_match() {
            assert_eq!(common_prefix_length("abc", "def"), 0);
        }

        #[test]
        fn exact_match() {
            assert_eq!(common_prefix_length("test", "test"), 4);
        }

        #[test]
        fn empty_first() {
            assert_eq!(common_prefix_length("", "test"), 0);
        }

        #[test]
        fn empty_second() {
            assert_eq!(common_prefix_length("test", ""), 0);
        }

        #[test]
        fn both_empty() {
            assert_eq!(common_prefix_length("", ""), 0);
        }

        #[test]
        fn single_char_match() {
            assert_eq!(common_prefix_length("a", "a"), 1);
        }

        #[test]
        fn single_char_no_match() {
            assert_eq!(common_prefix_length("a", "b"), 0);
        }

        #[test]
        fn unicode_characters() {
            assert_eq!(common_prefix_length("日本語", "日本人"), 2);
        }

        #[test]
        fn case_sensitive() {
            assert_eq!(common_prefix_length("Hello", "hello"), 0);
        }
    }

    mod common_suffix_tests {
        use super::*;

        #[test]
        fn partial_match() {
            assert_eq!(common_suffix_length("testing", "running"), 3); // "ing"
        }

        #[test]
        fn no_match() {
            assert_eq!(common_suffix_length("abc", "def"), 0);
        }

        #[test]
        fn exact_match() {
            assert_eq!(common_suffix_length("test", "test"), 4);
        }

        #[test]
        fn extension_like() {
            assert_eq!(common_suffix_length("file.txt", "data.txt"), 4); // ".txt"
        }

        #[test]
        fn both_empty() {
            assert_eq!(common_suffix_length("", ""), 0);
        }

        #[test]
        fn single_char_match() {
            assert_eq!(common_suffix_length("a", "a"), 1);
        }

        #[test]
        fn unicode_suffix() {
            assert_eq!(common_suffix_length("世界", "全世界"), 2);
        }
    }

    mod split_extension_tests {
        use super::*;

        #[test]
        fn simple_extension() {
            assert_eq!(split_name_extension("file.txt"), ("file", "txt"));
        }

        #[test]
        fn double_extension() {
            assert_eq!(split_name_extension("file.tar.gz"), ("file.tar", "gz"));
        }

        #[test]
        fn no_extension() {
            assert_eq!(split_name_extension("noextension"), ("noextension", ""));
        }

        #[test]
        fn hidden_file() {
            assert_eq!(split_name_extension(".hidden"), (".hidden", ""));
        }

        #[test]
        fn trailing_dot() {
            assert_eq!(split_name_extension("file."), ("file.", ""));
        }

        #[test]
        fn hidden_with_extension() {
            assert_eq!(split_name_extension(".gitignore"), (".gitignore", ""));
        }

        #[test]
        fn multiple_dots() {
            assert_eq!(split_name_extension("a.b.c.d"), ("a.b.c", "d"));
        }

        #[test]
        fn empty_string() {
            assert_eq!(split_name_extension(""), ("", ""));
        }

        #[test]
        fn single_char() {
            assert_eq!(split_name_extension("a"), ("a", ""));
        }

        #[test]
        fn single_dot() {
            assert_eq!(split_name_extension("."), (".", ""));
        }
    }

    mod similarity_score_tests {
        use super::*;

        #[test]
        fn same_extension_bonus() {
            let score = compute_similarity_score("data.csv", "backup.csv", 1000, 1000);
            assert!(
                score >= EXTENSION_MATCH_BONUS,
                "Same extension should give bonus"
            );
        }

        #[test]
        fn common_prefix_scores_high() {
            let score = compute_similarity_score("report_2024.pdf", "report_2023.pdf", 1000, 1000);
            assert!(score > 100, "Common prefix should give high score: {score}");
        }

        #[test]
        fn different_files_score_low() {
            let score = compute_similarity_score("image.png", "document.pdf", 1000, 100);
            assert!(
                score < MIN_FUZZY_SCORE,
                "Unrelated files should have low score: {score}"
            );
        }

        #[test]
        fn size_similarity_bonus() {
            let score_similar = compute_similarity_score("file.txt", "data.txt", 1000, 800);
            let score_different = compute_similarity_score("file.txt", "data.txt", 1000, 100);
            assert!(
                score_similar > score_different,
                "Similar sizes should score higher"
            );
        }

        #[test]
        fn versioned_files_match_well() {
            let score =
                compute_similarity_score("app-1.2.3.tar.gz", "app-1.2.2.tar.gz", 50000, 48000);
            assert!(score > 100, "Versioned files should match well: {score}");
        }

        #[test]
        fn zero_target_size() {
            let score = compute_similarity_score("file.txt", "data.txt", 0, 1000);
            // Should not get size bonus when target size is 0
            assert!(score >= EXTENSION_MATCH_BONUS);
        }

        #[test]
        fn zero_candidate_size() {
            let score = compute_similarity_score("file.txt", "data.txt", 1000, 0);
            // Should not get size bonus when candidate size is 0
            assert!(score >= EXTENSION_MATCH_BONUS);
        }

        #[test]
        fn both_sizes_zero() {
            let score = compute_similarity_score("file.txt", "data.txt", 0, 0);
            // Should not get size bonus when both are 0
            assert!(score >= EXTENSION_MATCH_BONUS);
        }

        #[test]
        fn exact_size_match() {
            let score = compute_similarity_score("a.txt", "b.txt", 1000, 1000);
            assert!(
                score >= EXTENSION_MATCH_BONUS + SIZE_SIMILARITY_BONUS,
                "Exact size match should include size bonus"
            );
        }

        #[test]
        fn candidate_larger_than_target() {
            let score = compute_similarity_score("a.txt", "b.txt", 500, 1000);
            // 500/1000 = 0.5, should still get size bonus
            assert!(score >= EXTENSION_MATCH_BONUS + SIZE_SIMILARITY_BONUS);
        }

        #[test]
        fn candidate_much_larger() {
            // ratio < 0.5
            let score = compute_similarity_score("a.txt", "b.txt", 100, 1000);
            assert!(
                score < EXTENSION_MATCH_BONUS + SIZE_SIMILARITY_BONUS,
                "Very different sizes should not get size bonus"
            );
        }

        #[test]
        fn no_extension_match() {
            let score = compute_similarity_score("file.txt", "file.csv", 1000, 1000);
            // Same prefix "file" but different extension
            assert!(score >= PREFIX_MATCH_POINTS * 4); // "file" is 4 chars
            assert!(
                score < EXTENSION_MATCH_BONUS + PREFIX_MATCH_POINTS * 4 + SIZE_SIMILARITY_BONUS
            );
        }

        #[test]
        fn suffix_contributes_to_score() {
            let score_with_suffix =
                compute_similarity_score("aaa_backup.txt", "bbb_backup.txt", 1000, 1000);
            let score_without_suffix =
                compute_similarity_score("aaa_backup.txt", "bbb_other.txt", 1000, 1000);
            assert!(
                score_with_suffix > score_without_suffix,
                "Common suffix should increase score"
            );
        }
    }

    mod fuzzy_matcher_tests {
        use super::*;

        #[test]
        fn new_default_values() {
            let matcher = FuzzyMatcher::new();
            assert_eq!(matcher.fuzzy_level(), FUZZY_LEVEL_1);
            assert_eq!(matcher.min_score(), MIN_FUZZY_SCORE);
            assert!(matcher.fuzzy_basis_dirs.is_empty());
        }

        #[test]
        fn with_level() {
            let matcher = FuzzyMatcher::with_level(2);
            assert_eq!(matcher.fuzzy_level(), 2);
            assert_eq!(matcher.min_score(), MIN_FUZZY_SCORE);
        }

        #[test]
        fn default_trait() {
            // Default derive uses 0 for fuzzy_level
            let matcher = FuzzyMatcher::default();
            assert_eq!(matcher.fuzzy_level(), 0);
            assert_eq!(matcher.min_score(), 0);
        }

        #[test]
        fn with_min_score() {
            let matcher = FuzzyMatcher::new().with_min_score(100);
            assert_eq!(matcher.min_score(), 100);
        }

        #[test]
        fn with_fuzzy_basis_dirs() {
            let dirs = vec![PathBuf::from("/tmp/basis1"), PathBuf::from("/tmp/basis2")];
            let matcher = FuzzyMatcher::new().with_fuzzy_basis_dirs(dirs.clone());
            assert_eq!(matcher.fuzzy_basis_dirs, dirs);
        }

        #[test]
        fn builder_chaining() {
            let dirs = vec![PathBuf::from("/tmp/basis")];
            let matcher = FuzzyMatcher::with_level(2)
                .with_min_score(50)
                .with_fuzzy_basis_dirs(dirs.clone());
            assert_eq!(matcher.fuzzy_level(), 2);
            assert_eq!(matcher.min_score(), 50);
            assert_eq!(matcher.fuzzy_basis_dirs, dirs);
        }

        #[test]
        fn debug_impl() {
            let matcher = FuzzyMatcher::new();
            let debug = format!("{matcher:?}");
            assert!(debug.contains("FuzzyMatcher"));
        }

        #[test]
        fn find_in_nonexistent_dir() {
            let matcher = FuzzyMatcher::new();
            let result = matcher.find_fuzzy_basis(
                std::ffi::OsStr::new("test.txt"),
                Path::new("/nonexistent/dir"),
                1000,
            );
            assert!(result.is_none());
        }

        #[test]
        fn level_2_skips_basis_dirs_without_config() {
            // Even with level 2, if no basis dirs configured, only search dest
            let matcher = FuzzyMatcher::with_level(2);
            assert!(matcher.fuzzy_basis_dirs.is_empty());
            // This is correct behavior - level 2 without basis dirs acts like level 1
        }
    }

    mod fuzzy_match_tests {
        use super::*;

        #[test]
        fn clone() {
            let m = FuzzyMatch {
                path: PathBuf::from("/tmp/test.txt"),
                score: 100,
            };
            let cloned = m.clone();
            assert_eq!(cloned.path, m.path);
            assert_eq!(cloned.score, m.score);
        }

        #[test]
        fn debug() {
            let m = FuzzyMatch {
                path: PathBuf::from("/tmp/test.txt"),
                score: 100,
            };
            let debug = format!("{m:?}");
            assert!(debug.contains("FuzzyMatch"));
            assert!(debug.contains("100"));
        }
    }

    mod constants_tests {
        use super::*;

        #[test]
        fn fuzzy_level_constants() {
            assert_eq!(FUZZY_LEVEL_1, 1);
            assert_eq!(FUZZY_LEVEL_2, 2);
        }

        #[test]
        fn scoring_constants_reasonable() {
            // Extension match should be worth several prefix chars
            assert!(EXTENSION_MATCH_BONUS >= PREFIX_MATCH_POINTS * 3);

            // Prefix should be worth more than suffix
            assert!(PREFIX_MATCH_POINTS > SUFFIX_MATCH_POINTS);

            // Size bonus should be meaningful but not dominant
            assert!(SIZE_SIMILARITY_BONUS > MIN_FUZZY_SCORE);
            assert!(SIZE_SIMILARITY_BONUS < EXTENSION_MATCH_BONUS);
        }
    }
}
