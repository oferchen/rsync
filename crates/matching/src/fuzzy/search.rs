//! Directory search logic for fuzzy basis file matching.
//!
//! Implements the directory scanning and best-match selection that
//! `FuzzyMatcher` uses to find similar basis files.
//!
//! # Upstream Reference
//!
//! Mirrors `generator.c:1580` - `find_fuzzy_basis()`.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use super::scoring::compute_similarity_score;
use super::{FUZZY_LEVEL_2, FuzzyMatch, FuzzyMatcher};

impl FuzzyMatcher {
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

        if let Some(m) = search_directory(dest_dir, &target_name_str, target_size, self.min_score) {
            update_best_match(&mut best_match, m, self.min_score);
        }

        if self.fuzzy_level >= FUZZY_LEVEL_2 {
            for basis_dir in &self.fuzzy_basis_dirs {
                if let Some(m) =
                    search_directory(basis_dir, &target_name_str, target_size, self.min_score)
                {
                    update_best_match(&mut best_match, m, self.min_score);
                }
            }
        }

        best_match
    }
}

/// Searches a single directory for fuzzy matches.
///
/// - Skips directories and special files
/// - Skips exact name matches (those are not fuzzy)
/// - Returns the highest-scoring candidate from this directory
fn search_directory(
    dir: &Path,
    target_name: &str,
    target_size: u64,
    min_score: u32,
) -> Option<FuzzyMatch> {
    let Ok(entries) = fs::read_dir(dir) else {
        return None;
    };

    let mut best_match: Option<FuzzyMatch> = None;

    for entry in entries.flatten() {
        let path = entry.path();

        let metadata = match entry.metadata() {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };

        let candidate_name = match path.file_name() {
            Some(name) => name.to_string_lossy(),
            None => continue,
        };

        // Skip the exact-name match: fuzzy matching only fires when no
        // identically-named file exists in the destination.
        if candidate_name == target_name {
            continue;
        }

        let score =
            compute_similarity_score(target_name, &candidate_name, target_size, metadata.len());

        if score >= min_score {
            update_best_match(&mut best_match, FuzzyMatch { path, score }, min_score);
        }
    }

    best_match
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzy::{FUZZY_LEVEL_1, MIN_FUZZY_SCORE};
    use std::path::PathBuf;

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
            // Derived Default leaves both fields at 0; FuzzyMatcher::new() is
            // the supported way to obtain a usable level-1 matcher.
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
            // Level 2 without configured basis dirs degenerates to level 1
            // behaviour (search the destination directory only).
            let matcher = FuzzyMatcher::with_level(2);
            assert!(matcher.fuzzy_basis_dirs.is_empty());
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
