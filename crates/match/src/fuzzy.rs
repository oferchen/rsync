//! crates/match/src/fuzzy.rs
//!
//! Fuzzy basis file matching for delta transfers.
//!
//! When `--fuzzy` is enabled, this module searches the destination directory
//! for files with similar names that can serve as basis files for delta
//! transfer, reducing data transfer even when no exact match exists.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Score threshold for considering a file as a fuzzy match.
/// Files must score above this to be considered as basis candidates.
const MIN_FUZZY_SCORE: u32 = 10;

/// Bonus score for matching file extension.
const EXTENSION_MATCH_BONUS: u32 = 50;

/// Points per character of matching prefix.
const PREFIX_MATCH_POINTS: u32 = 10;

/// Points per character of matching suffix (before extension).
const SUFFIX_MATCH_POINTS: u32 = 8;

/// Bonus for similar file size (within 50% of target).
const SIZE_SIMILARITY_BONUS: u32 = 30;

/// Result of fuzzy matching search.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    /// Path to the matching basis file.
    pub path: PathBuf,
    /// Similarity score (higher is better).
    pub score: u32,
}

/// Fuzzy matcher for finding similar basis files.
#[derive(Debug, Default)]
pub struct FuzzyMatcher {
    /// Minimum score required for a match.
    min_score: u32,
    /// Additional directories to search (for -yy mode).
    fuzzy_basis_dirs: Vec<PathBuf>,
}

impl FuzzyMatcher {
    /// Creates a new fuzzy matcher with default settings.
    pub fn new() -> Self {
        Self {
            min_score: MIN_FUZZY_SCORE,
            fuzzy_basis_dirs: Vec::new(),
        }
    }

    /// Sets additional fuzzy basis directories (for `-yy` mode).
    pub fn with_fuzzy_basis_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.fuzzy_basis_dirs = dirs;
        self
    }

    /// Sets the minimum score threshold.
    pub fn with_min_score(mut self, score: u32) -> Self {
        self.min_score = score;
        self
    }

    /// Finds the best fuzzy match for a file in the destination directory.
    ///
    /// Searches the parent directory of `dest_path` for files with similar
    /// names. If `--fuzzy` is specified twice (`-yy`), also searches the
    /// configured `fuzzy_basis_dirs`.
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
    pub fn find_fuzzy_basis(
        &self,
        target_name: &OsStr,
        dest_dir: &Path,
        target_size: u64,
    ) -> Option<FuzzyMatch> {
        let target_name_str = target_name.to_string_lossy();

        let mut best_match: Option<FuzzyMatch> = None;

        // Search destination directory
        if let Some(m) = self.search_directory(dest_dir, &target_name_str, target_size) {
            if m.score >= self.min_score {
                best_match = Some(m);
            }
        }

        // Search additional fuzzy basis directories if configured
        for basis_dir in &self.fuzzy_basis_dirs {
            if let Some(m) = self.search_directory(basis_dir, &target_name_str, target_size) {
                if m.score >= self.min_score {
                    match &best_match {
                        Some(existing) if existing.score >= m.score => {}
                        _ => best_match = Some(m),
                    }
                }
            }
        }

        best_match
    }

    /// Searches a single directory for fuzzy matches.
    fn search_directory(
        &self,
        dir: &Path,
        target_name: &str,
        target_size: u64,
    ) -> Option<FuzzyMatch> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return None,
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
                match &best_match {
                    Some(existing) if existing.score >= score => {}
                    _ => {
                        best_match = Some(FuzzyMatch { path, score });
                    }
                }
            }
        }

        best_match
    }
}

/// Computes a similarity score between two filenames.
///
/// The scoring algorithm considers:
/// - Common prefix length (higher weight)
/// - Common suffix length (medium weight)
/// - Matching file extension (bonus)
/// - Similar file sizes (bonus)
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

        // Bonus if sizes are within 50% of each other
        if size_ratio >= 0.5 {
            score += SIZE_SIMILARITY_BONUS;
        }
    }

    score
}

/// Splits a filename into base name and extension.
fn split_name_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(pos) if pos > 0 && pos < name.len() - 1 => (&name[..pos], &name[pos + 1..]),
        _ => (name, ""),
    }
}

/// Computes the length of the common prefix between two strings.
fn common_prefix_length(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .count()
}

/// Computes the length of the common suffix between two strings.
fn common_suffix_length(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(ca, cb)| ca == cb)
        .count()
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
            assert_eq!(matcher.min_score, MIN_FUZZY_SCORE);
            assert!(matcher.fuzzy_basis_dirs.is_empty());
        }

        #[test]
        fn default_trait() {
            // Default derive uses 0, not MIN_FUZZY_SCORE
            let matcher = FuzzyMatcher::default();
            assert_eq!(matcher.min_score, 0);
        }

        #[test]
        fn with_min_score() {
            let matcher = FuzzyMatcher::new().with_min_score(100);
            assert_eq!(matcher.min_score, 100);
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
            let matcher = FuzzyMatcher::new()
                .with_min_score(50)
                .with_fuzzy_basis_dirs(dirs.clone());
            assert_eq!(matcher.min_score, 50);
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
}
