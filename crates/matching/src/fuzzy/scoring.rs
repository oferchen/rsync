//! Similarity scoring algorithm for fuzzy basis file matching.
//!
//! Computes a multi-factor similarity score between two filenames,
//! considering extension, prefix, suffix, and file size.
//!
//! # Upstream Reference
//!
//! Mirrors the scoring logic in `generator.c:1450-1530` - `fuzzy_find()`.

use super::{
    EXTENSION_MATCH_BONUS, PREFIX_MATCH_POINTS, SIZE_SIMILARITY_BONUS, SUFFIX_MATCH_POINTS,
};

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

    let (target_base, target_ext) = split_name_extension(target);
    let (candidate_base, candidate_ext) = split_name_extension(candidate);

    if target_ext == candidate_ext && !target_ext.is_empty() {
        score += EXTENSION_MATCH_BONUS;
    }

    let prefix_len = common_prefix_length(target_base, candidate_base);
    score += prefix_len as u32 * PREFIX_MATCH_POINTS;

    let suffix_len = common_suffix_length(target_base, candidate_base);
    score += suffix_len as u32 * SUFFIX_MATCH_POINTS;

    if target_size > 0 && candidate_size > 0 {
        let size_ratio = if target_size >= candidate_size {
            candidate_size as f64 / target_size as f64
        } else {
            target_size as f64 / candidate_size as f64
        };

        // upstream rsync's size-similarity heuristic: award a bonus when
        // sizes are within 50% of each other (ratio >= 0.5).
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

    // Fast path: byte-wise compare; ASCII filenames hit this branch.
    let min_len = a_bytes.len().min(b_bytes.len());
    let mut common_bytes = 0;

    for i in 0..min_len {
        if a_bytes[i] != b_bytes[i] {
            break;
        }
        common_bytes = i + 1;
    }

    // If the byte-prefix lands on a UTF-8 boundary in both strings the char
    // count is exact; otherwise fall back to a Unicode-aware comparison so we
    // never split a multibyte sequence.
    if a.is_char_boundary(common_bytes) && b.is_char_boundary(common_bytes) {
        a[..common_bytes].chars().count()
    } else {
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

    // Fast path: byte-wise compare from the tail; ASCII filenames hit this branch.
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

    let a_start = a_bytes.len() - common_bytes;
    let b_start = b_bytes.len() - common_bytes;
    if a.is_char_boundary(a_start) && b.is_char_boundary(b_start) {
        a[a_start..].chars().count()
    } else {
        // Byte-suffix straddles a multibyte sequence; fall back to a
        // Unicode-aware reverse comparison.
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
            assert_eq!(common_suffix_length("testing", "running"), 3);
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
            assert_eq!(common_suffix_length("file.txt", "data.txt"), 4);
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
        use crate::fuzzy::{MIN_FUZZY_SCORE, SIZE_SIMILARITY_BONUS};

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
            assert!(score >= EXTENSION_MATCH_BONUS);
        }

        #[test]
        fn zero_candidate_size() {
            let score = compute_similarity_score("file.txt", "data.txt", 1000, 0);
            assert!(score >= EXTENSION_MATCH_BONUS);
        }

        #[test]
        fn both_sizes_zero() {
            let score = compute_similarity_score("file.txt", "data.txt", 0, 0);
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
            // 500/1000 = 0.5 lands exactly on the bonus threshold.
            let score = compute_similarity_score("a.txt", "b.txt", 500, 1000);
            assert!(score >= EXTENSION_MATCH_BONUS + SIZE_SIMILARITY_BONUS);
        }

        #[test]
        fn candidate_much_larger() {
            // 100/1000 = 0.1 falls below the bonus threshold.
            let score = compute_similarity_score("a.txt", "b.txt", 100, 1000);
            assert!(
                score < EXTENSION_MATCH_BONUS + SIZE_SIMILARITY_BONUS,
                "Very different sizes should not get size bonus"
            );
        }

        #[test]
        fn no_extension_match() {
            // Common prefix "file" (4 chars) but different extension.
            let score = compute_similarity_score("file.txt", "file.csv", 1000, 1000);
            assert!(score >= PREFIX_MATCH_POINTS * 4);
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
}
