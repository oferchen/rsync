//! crates/engine/src/fuzzy.rs
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

    #[test]
    fn test_common_prefix_length() {
        assert_eq!(common_prefix_length("hello", "help"), 3);
        assert_eq!(common_prefix_length("abc", "def"), 0);
        assert_eq!(common_prefix_length("test", "test"), 4);
        assert_eq!(common_prefix_length("", "test"), 0);
        assert_eq!(common_prefix_length("test", ""), 0);
    }

    #[test]
    fn test_common_suffix_length() {
        assert_eq!(common_suffix_length("testing", "running"), 3); // "ing"
        assert_eq!(common_suffix_length("abc", "def"), 0);
        assert_eq!(common_suffix_length("test", "test"), 4);
        assert_eq!(common_suffix_length("file.txt", "data.txt"), 4); // ".txt"
    }

    #[test]
    fn test_split_name_extension() {
        assert_eq!(split_name_extension("file.txt"), ("file", "txt"));
        assert_eq!(split_name_extension("file.tar.gz"), ("file.tar", "gz"));
        assert_eq!(split_name_extension("noextension"), ("noextension", ""));
        assert_eq!(split_name_extension(".hidden"), (".hidden", ""));
        assert_eq!(split_name_extension("file."), ("file.", ""));
    }

    #[test]
    fn test_similarity_score_same_extension() {
        let score = compute_similarity_score("data.csv", "backup.csv", 1000, 1000);
        assert!(
            score >= EXTENSION_MATCH_BONUS,
            "Same extension should give bonus"
        );
    }

    #[test]
    fn test_similarity_score_common_prefix() {
        let score = compute_similarity_score("report_2024.pdf", "report_2023.pdf", 1000, 1000);
        // Should have: extension match (50) + prefix "report_202" (70) + suffix ".pdf" handled by ext
        assert!(score > 100, "Common prefix should give high score: {score}");
    }

    #[test]
    fn test_similarity_score_different_files() {
        // Use very different sizes to avoid size similarity bonus (ratio < 0.5)
        let score = compute_similarity_score("image.png", "document.pdf", 1000, 100);
        // Different extension, no common prefix/suffix, no size bonus
        assert!(
            score < MIN_FUZZY_SCORE,
            "Unrelated files should have low score: {score}"
        );
    }

    #[test]
    fn test_similarity_score_size_bonus() {
        let score_similar = compute_similarity_score("file.txt", "data.txt", 1000, 800);
        let score_different = compute_similarity_score("file.txt", "data.txt", 1000, 100);

        assert!(
            score_similar > score_different,
            "Similar sizes should score higher"
        );
    }

    #[test]
    fn test_similarity_score_versioned_files() {
        // Common pattern: versioned files
        let score = compute_similarity_score("app-1.2.3.tar.gz", "app-1.2.2.tar.gz", 50000, 48000);
        assert!(score > 100, "Versioned files should match well: {score}");
    }

    #[test]
    fn test_fuzzy_matcher_min_score_threshold() {
        let matcher = FuzzyMatcher::new().with_min_score(100);
        assert_eq!(matcher.min_score, 100);
    }

    #[test]
    fn test_fuzzy_matcher_basis_dirs() {
        let dirs = vec![PathBuf::from("/tmp/basis1"), PathBuf::from("/tmp/basis2")];
        let matcher = FuzzyMatcher::new().with_fuzzy_basis_dirs(dirs.clone());
        assert_eq!(matcher.fuzzy_basis_dirs, dirs);
    }
}
