//! Comprehensive tests for --exclude-from file parsing.
//!
//! These tests verify the behavior of reading exclude patterns from files as specified
//! via rsync's `--exclude-from=FILE` flag. The tests cover:
//!
//! 1. Reading rules from a single file
//! 2. Comment handling (# and ; prefixes)
//! 3. Blank line handling (empty lines, whitespace-only lines)
//! 4. Multiple --exclude-from files
//! 5. Error handling for missing files
//! 6. Line ending handling (LF, CRLF)
//! 7. Pattern preservation (leading whitespace, special characters)
//! 8. Reading from standard input (dash path)
//! 9. Large file handling
//!
//! Reference: rsync 3.4.1 exclude.c and rsync(1) man page FILTER RULES section

use filters::{FilterAction, FilterSet, read_rules, read_rules_recursive};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

// ============================================================================
// 1. Reading Rules from a Single File
// ============================================================================

mod reading_from_file {
    use super::*;

    /// Test: Basic file reading with simple patterns.
    /// Note: Filter files use rsync filter rule syntax (- for exclude, + for include).
    #[test]
    fn reads_simple_patterns_from_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n- *.bak\n- *.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
        assert_eq!(rules[2].pattern(), "*.log");
    }

    /// Test: File with mixed rule types (short form).
    #[test]
    fn reads_mixed_rule_types() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("filters.txt");
        fs::write(&path, "+ *.rs\n- *.tmp\nP /important\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert_eq!(rules[1].action(), FilterAction::Exclude);
        assert_eq!(rules[2].action(), FilterAction::Protect);
    }

    /// Test: File with long form rule syntax.
    #[test]
    fn reads_long_form_rules() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("filters.txt");
        fs::write(&path, "include *.txt\nexclude *.bak\nprotect /data\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert_eq!(rules[0].pattern(), "*.txt");
        assert_eq!(rules[1].action(), FilterAction::Exclude);
        assert_eq!(rules[2].action(), FilterAction::Protect);
    }

    /// Test: File with directory patterns.
    #[test]
    fn reads_directory_patterns() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- build/\n- node_modules/\n- .git/\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        let set = FilterSet::from_rules(rules).expect("compile");

        assert!(!set.allows(Path::new("build"), true));
        assert!(!set.allows(Path::new("node_modules"), true));
        assert!(!set.allows(Path::new(".git"), true));
    }

    /// Test: File with anchored patterns.
    #[test]
    fn reads_anchored_patterns() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- /root.txt\n- /config/\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        let set = FilterSet::from_rules(rules).expect("compile");

        // Anchored patterns only match at root
        assert!(!set.allows(Path::new("root.txt"), false));
        assert!(set.allows(Path::new("dir/root.txt"), false));
    }

    /// Test: File with double-star patterns.
    #[test]
    fn reads_double_star_patterns() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- **/cache/**\n- **/*.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        let set = FilterSet::from_rules(rules).expect("compile");

        assert!(!set.allows(Path::new("app/cache/data"), false));
        assert!(!set.allows(Path::new("logs/debug.log"), false));
    }

    /// Test: File with character class patterns.
    #[test]
    fn reads_character_class_patterns() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- file[0-9].txt\n- *.[ch]\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        let set = FilterSet::from_rules(rules).expect("compile");

        assert!(!set.allows(Path::new("file1.txt"), false));
        assert!(!set.allows(Path::new("main.c"), false));
        assert!(!set.allows(Path::new("header.h"), false));
    }

    /// Test: Single pattern file.
    #[test]
    fn reads_single_pattern_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern(), "*.tmp");
    }

    /// Test: Empty file returns no rules.
    #[test]
    fn empty_file_returns_empty_rules() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("empty.txt");
        fs::write(&path, "").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert!(rules.is_empty());
    }
}

// ============================================================================
// 2. Comment Handling (# and ; prefixes)
// ============================================================================

mod comment_handling {
    use super::*;

    /// Test: Lines starting with # are comments (hash comments).
    #[test]
    fn skips_hash_comments() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(
            &path,
            "# This is a comment\n- *.tmp\n# Another comment\n- *.bak\n",
        )
        .expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
    }

    /// Test: Lines starting with ; are comments (semicolon comments).
    #[test]
    fn skips_semicolon_comments() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(
            &path,
            "; This is a comment\n- *.tmp\n; Another comment\n- *.bak\n",
        )
        .expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
    }

    /// Test: Mixed hash and semicolon comments.
    #[test]
    fn mixed_comment_styles() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(
            &path,
            "# Hash comment\n- *.tmp\n; Semicolon comment\n- *.bak\n",
        )
        .expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
    }

    /// Test: Comment with leading whitespace is still a comment.
    #[test]
    fn comment_with_leading_whitespace() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "  # Comment with leading spaces\n- *.tmp\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern(), "*.tmp");
    }

    /// Test: File with only comments returns no rules.
    #[test]
    fn only_comments_returns_empty() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(
            &path,
            "# Comment 1\n; Comment 2\n# Comment 3\n; Comment 4\n",
        )
        .expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert!(rules.is_empty());
    }

    /// Test: Inline comments are NOT supported (matches upstream).
    /// Pattern `*.tmp # comment` should include the comment as part of pattern.
    #[test]
    fn inline_comments_not_supported() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp # this is part of pattern\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        // The comment is part of the pattern - rsync doesn't support inline comments
        assert_eq!(rules[0].pattern(), "*.tmp # this is part of pattern");
    }

    /// Test: Comment at end of file without newline.
    #[test]
    fn comment_at_end_without_newline() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n# trailing comment").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern(), "*.tmp");
    }

    /// Test: Comment character inside pattern (not at start).
    #[test]
    fn comment_char_inside_pattern() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- file#1.txt\n- file;2.txt\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "file#1.txt");
        assert_eq!(rules[1].pattern(), "file;2.txt");
    }
}

// ============================================================================
// 3. Blank Line Handling
// ============================================================================

mod blank_line_handling {
    use super::*;

    /// Test: Empty lines are skipped.
    #[test]
    fn skips_empty_lines() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n\n- *.bak\n\n\n- *.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
        assert_eq!(rules[2].pattern(), "*.log");
    }

    /// Test: Whitespace-only lines are skipped.
    #[test]
    fn skips_whitespace_only_lines() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n   \n- *.bak\n\t\t\n- *.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
    }

    /// Test: Lines with only tabs are skipped.
    #[test]
    fn skips_tab_only_lines() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n\t\n- *.bak\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
    }

    /// Test: File with blank lines at start.
    #[test]
    fn blank_lines_at_start() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "\n\n\n- *.tmp\n- *.bak\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
    }

    /// Test: File with blank lines at end.
    #[test]
    fn blank_lines_at_end() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n- *.bak\n\n\n\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
    }

    /// Test: File with only blank lines.
    #[test]
    fn only_blank_lines() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "\n\n   \n\t\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert!(rules.is_empty());
    }

    /// Test: Mixed blank lines and comments.
    #[test]
    fn mixed_blank_lines_and_comments() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "\n# Comment\n\n; Another comment\n   \n- *.tmp\n\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern(), "*.tmp");
    }
}

// ============================================================================
// 4. Multiple --exclude-from Files
// ============================================================================

mod multiple_exclude_from_files {
    use super::*;

    /// Test: Rules from multiple files are combined.
    #[test]
    fn combines_rules_from_multiple_files() {
        let dir = tempdir().expect("tempdir");

        let path1 = dir.path().join("excludes1.txt");
        let path2 = dir.path().join("excludes2.txt");
        fs::write(&path1, "- *.tmp\n- *.bak\n").expect("write");
        fs::write(&path2, "- *.log\n- *.swp\n").expect("write");

        let rules1 = read_rules(&path1).expect("read rules 1");
        let rules2 = read_rules(&path2).expect("read rules 2");

        let mut all_rules = rules1;
        all_rules.extend(rules2);

        assert_eq!(all_rules.len(), 4);
        assert_eq!(all_rules[0].pattern(), "*.tmp");
        assert_eq!(all_rules[1].pattern(), "*.bak");
        assert_eq!(all_rules[2].pattern(), "*.log");
        assert_eq!(all_rules[3].pattern(), "*.swp");
    }

    /// Test: Order of files matters (first-match-wins).
    #[test]
    fn file_order_matters() {
        let dir = tempdir().expect("tempdir");

        let path1 = dir.path().join("includes.txt");
        let path2 = dir.path().join("excludes.txt");
        fs::write(&path1, "+ important.txt\n").expect("write");
        fs::write(&path2, "- *.txt\n").expect("write");

        let rules1 = read_rules(&path1).expect("read rules 1");
        let rules2 = read_rules(&path2).expect("read rules 2");

        let mut all_rules = rules1;
        all_rules.extend(rules2);

        let set = FilterSet::from_rules(all_rules).expect("compile");

        // important.txt matched first by include
        assert!(set.allows(Path::new("important.txt"), false));
        // other.txt matched by exclude
        assert!(!set.allows(Path::new("other.txt"), false));
    }

    /// Test: Same pattern in multiple files (duplicates allowed).
    #[test]
    fn duplicate_patterns_allowed() {
        let dir = tempdir().expect("tempdir");

        let path1 = dir.path().join("file1.txt");
        let path2 = dir.path().join("file2.txt");
        fs::write(&path1, "- *.tmp\n").expect("write");
        fs::write(&path2, "- *.tmp\n").expect("write");

        let rules1 = read_rules(&path1).expect("read rules 1");
        let rules2 = read_rules(&path2).expect("read rules 2");

        let mut all_rules = rules1;
        all_rules.extend(rules2);

        // Both rules are kept (duplicates not deduplicated at read time)
        assert_eq!(all_rules.len(), 2);
    }

    /// Test: Clear rule in second file.
    #[test]
    fn clear_in_second_file() {
        let dir = tempdir().expect("tempdir");

        let path1 = dir.path().join("file1.txt");
        let path2 = dir.path().join("file2.txt");
        fs::write(&path1, "- *.tmp\n- *.bak\n").expect("write");
        fs::write(&path2, "!\n- *.log\n").expect("write");

        let rules1 = read_rules(&path1).expect("read rules 1");
        let rules2 = read_rules(&path2).expect("read rules 2");

        let mut all_rules = rules1;
        all_rules.extend(rules2);

        let set = FilterSet::from_rules(all_rules).expect("compile");

        // Rules before clear are cleared
        assert!(set.allows(Path::new("file.tmp"), false));
        assert!(set.allows(Path::new("file.bak"), false));
        // Rule after clear is active
        assert!(!set.allows(Path::new("file.log"), false));
    }

    /// Test: Empty first file, rules in second.
    #[test]
    fn empty_first_file() {
        let dir = tempdir().expect("tempdir");

        let path1 = dir.path().join("empty.txt");
        let path2 = dir.path().join("excludes.txt");
        fs::write(&path1, "").expect("write");
        fs::write(&path2, "- *.tmp\n").expect("write");

        let rules1 = read_rules(&path1).expect("read rules 1");
        let rules2 = read_rules(&path2).expect("read rules 2");

        let mut all_rules = rules1;
        all_rules.extend(rules2);

        assert_eq!(all_rules.len(), 1);
        assert_eq!(all_rules[0].pattern(), "*.tmp");
    }

    /// Test: Conflicting rules across files (include then exclude).
    #[test]
    fn conflicting_rules_across_files() {
        let dir = tempdir().expect("tempdir");

        // File 1: include all txt
        let path1 = dir.path().join("includes.txt");
        fs::write(&path1, "+ *.txt\n").expect("write");

        // File 2: exclude secret.txt
        let path2 = dir.path().join("excludes.txt");
        fs::write(&path2, "- secret.txt\n").expect("write");

        let rules1 = read_rules(&path1).expect("read rules 1");
        let rules2 = read_rules(&path2).expect("read rules 2");

        let mut all_rules = rules1;
        all_rules.extend(rules2);

        let set = FilterSet::from_rules(all_rules).expect("compile");

        // Include rule comes first, so all txt files are included (first-match-wins)
        assert!(set.allows(Path::new("readme.txt"), false));
        assert!(set.allows(Path::new("secret.txt"), false)); // Include wins
    }
}

// ============================================================================
// 5. Error Handling for Missing Files
// ============================================================================

mod error_handling_missing_files {
    use super::*;

    /// Test: Reading a non-existent file returns an error.
    #[test]
    fn missing_file_returns_error() {
        let result = read_rules(Path::new("/nonexistent/path/file.txt"));
        assert!(result.is_err());
    }

    /// Test: Error message includes the file path.
    #[test]
    fn error_includes_path() {
        let result = read_rules(Path::new("/nonexistent/excludes.txt"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.path.contains("excludes.txt"));
    }

    /// Test: Reading a directory instead of file returns error.
    #[test]
    fn directory_instead_of_file_returns_error() {
        let dir = tempdir().expect("tempdir");
        let result = read_rules(dir.path());
        assert!(result.is_err());
    }

    /// Test: Permission denied error (if possible to create).
    #[test]
    #[cfg(unix)]
    fn unreadable_file_returns_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("unreadable.txt");
        fs::write(&path, "- *.tmp\n").expect("write");

        // Remove read permission
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&path, perms).expect("set permissions");

        let result = read_rules(&path);

        // Restore permissions for cleanup
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o644);
        let _ = fs::set_permissions(&path, perms);

        assert!(result.is_err());
    }
}

// ============================================================================
// 6. Line Ending Handling (LF, CRLF)
// ============================================================================

mod line_ending_handling {
    use super::*;

    /// Test: Unix line endings (LF).
    #[test]
    fn unix_line_endings() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n- *.bak\n- *.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
        assert_eq!(rules[2].pattern(), "*.log");
    }

    /// Test: Windows line endings (CRLF).
    #[test]
    fn windows_line_endings() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\r\n- *.bak\r\n- *.log\r\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
        assert_eq!(rules[2].pattern(), "*.log");
    }

    /// Test: Mixed line endings (LF and CRLF).
    #[test]
    fn mixed_line_endings() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n- *.bak\r\n- *.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
        assert_eq!(rules[2].pattern(), "*.log");
    }

    /// Test: Old Mac line endings (CR only) - handled as single line.
    #[test]
    fn old_mac_line_endings() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        // CR-only line endings result in one long line
        fs::write(&path, "- *.tmp\r- *.bak\r- *.log\r").expect("write");

        let rules = read_rules(&path).expect("read rules");
        // Old Mac CR-only is treated as one line (no proper line separation)
        // This is consistent with most Unix tools
        assert_eq!(rules.len(), 1);
    }

    /// Test: No trailing newline.
    #[test]
    fn no_trailing_newline() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\n- *.bak").expect("write"); // No newline after *.bak

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
    }

    /// Test: CRLF with no trailing newline.
    #[test]
    fn crlf_no_trailing_newline() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\r\n- *.bak").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
    }
}

// ============================================================================
// 7. Pattern Preservation
// ============================================================================

mod pattern_preservation {
    use super::*;

    /// Test: Leading whitespace in pattern is trimmed.
    /// Note: Unlike rsync's raw --exclude-from, filter rules parse patterns
    /// and leading whitespace between the action and pattern is consumed.
    #[test]
    fn leading_whitespace_trimmed() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        // Pattern with leading space (after the action)
        fs::write(&path, "-   spaced_file.txt\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        // Leading whitespace is trimmed by the parser
        assert_eq!(rules[0].pattern(), "spaced_file.txt");
    }

    /// Test: Trailing whitespace in pattern is trimmed.
    #[test]
    fn trailing_whitespace_trimmed() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- pattern_with_trailing   \n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        // Trailing whitespace is trimmed
        assert_eq!(rules[0].pattern(), "pattern_with_trailing");
    }

    /// Test: Pattern case is preserved.
    #[test]
    fn preserves_pattern_case() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- README.TXT\n- Makefile\n- CamelCase\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules[0].pattern(), "README.TXT");
        assert_eq!(rules[1].pattern(), "Makefile");
        assert_eq!(rules[2].pattern(), "CamelCase");
    }

    /// Test: Special glob characters are preserved.
    #[test]
    fn preserves_special_characters() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.[ch]\n- file[0-9]?.txt\n- **/*.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules[0].pattern(), "*.[ch]");
        assert_eq!(rules[1].pattern(), "file[0-9]?.txt");
        assert_eq!(rules[2].pattern(), "**/*.log");
    }

    /// Test: Escaped characters are preserved.
    #[test]
    fn preserves_escaped_characters() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- file\\*.txt\n- what\\?\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules[0].pattern(), "file\\*.txt");
        assert_eq!(rules[1].pattern(), "what\\?");
    }

    /// Test: Unicode patterns are preserved.
    #[test]
    fn preserves_unicode_patterns() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- \u{4e2d}\u{6587}.txt\n- caf\u{e9}.doc\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules[0].pattern(), "\u{4e2d}\u{6587}.txt"); // Chinese characters
        assert_eq!(rules[1].pattern(), "caf\u{e9}.doc"); // cafe with accent
    }
}

// ============================================================================
// 8. Merge File Functionality (. directive)
// ============================================================================

mod merge_file_functionality {
    use super::*;

    /// Test: Merge directive includes nested file.
    #[test]
    fn merge_directive_includes_nested_file() {
        let dir = tempdir().expect("tempdir");

        let nested_path = dir.path().join("nested.rules");
        fs::write(&nested_path, "- *.nested\n").expect("write");

        let main_path = dir.path().join("main.rules");
        fs::write(
            &main_path,
            format!("- *.main\n. {}\n- *.after\n", nested_path.display()),
        )
        .expect("write");

        let rules = read_rules_recursive(&main_path, 10).expect("read recursive");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.main");
        assert_eq!(rules[1].pattern(), "*.nested");
        assert_eq!(rules[2].pattern(), "*.after");
    }

    /// Test: Recursive merge depth limit.
    #[test]
    fn merge_depth_limit_enforced() {
        let dir = tempdir().expect("tempdir");

        // Create self-referencing file
        let path = dir.path().join("loop.rules");
        fs::write(&path, format!(". {}\n", path.display())).expect("write");

        let result = read_rules_recursive(&path, 5);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("depth"));
    }

    /// Test: Relative path merge.
    #[test]
    fn relative_path_merge() {
        let dir = tempdir().expect("tempdir");

        let nested_path = dir.path().join("subdir/nested.rules");
        fs::create_dir(dir.path().join("subdir")).expect("mkdir");
        fs::write(&nested_path, "- *.nested\n").expect("write");

        let main_path = dir.path().join("main.rules");
        fs::write(&main_path, ". subdir/nested.rules\n").expect("write");

        let rules = read_rules_recursive(&main_path, 10).expect("read recursive");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern(), "*.nested");
    }

    /// Test: Dir-merge rules are preserved (not expanded).
    #[test]
    fn dir_merge_preserved_not_expanded() {
        let dir = tempdir().expect("tempdir");

        let path = dir.path().join("rules.txt");
        fs::write(&path, ": .rsync-filter\n- *.tmp\n").expect("write");

        let rules = read_rules_recursive(&path, 10).expect("read recursive");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].action(), FilterAction::DirMerge);
        assert_eq!(rules[0].pattern(), ".rsync-filter");
        assert_eq!(rules[1].action(), FilterAction::Exclude);
    }
}

// ============================================================================
// 9. Large File Handling
// ============================================================================

mod large_file_handling {
    use super::*;

    /// Test: File with many patterns (stress test).
    #[test]
    fn many_patterns() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");

        // Generate 1000 patterns
        let mut content = String::new();
        for i in 0..1000 {
            content.push_str(&format!("- pattern_{i}.txt\n"));
        }
        fs::write(&path, &content).expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1000);
        assert_eq!(rules[0].pattern(), "pattern_0.txt");
        assert_eq!(rules[999].pattern(), "pattern_999.txt");
    }

    /// Test: Long pattern (PATH_MAX-ish length).
    #[test]
    fn long_pattern() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");

        let long_name = "x".repeat(1000);
        fs::write(&path, format!("- {long_name}\n")).expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern(), long_name);
    }

    /// Test: Many comments interspersed with patterns.
    #[test]
    fn many_comments_interspersed() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");

        let mut content = String::new();
        for i in 0..100 {
            content.push_str(&format!("# Comment {i}\n"));
            content.push_str(&format!("- pattern_{i}.txt\n"));
            content.push('\n'); // blank line
        }
        fs::write(&path, &content).expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 100);
    }
}

// ============================================================================
// 10. Rule Modifiers from File
// ============================================================================

mod rule_modifiers_from_file {
    use super::*;

    /// Test: Negation modifier from file.
    #[test]
    fn negation_modifier() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "-! *.txt\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_negated());
    }

    /// Test: Perishable modifier from file.
    #[test]
    fn perishable_modifier() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "-p *.tmp\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_perishable());
    }

    /// Test: Sender-only modifier from file.
    #[test]
    fn sender_only_modifier() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "-s *.bak\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    /// Test: Receiver-only modifier from file.
    #[test]
    fn receiver_only_modifier() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "-r *.bak\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert!(!rules[0].applies_to_sender());
        assert!(rules[0].applies_to_receiver());
    }

    /// Test: Combined modifiers from file.
    #[test]
    fn combined_modifiers() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "-!ps *.tmp\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_negated());
        assert!(rules[0].is_perishable());
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    /// Test: Word-split modifier from file.
    #[test]
    fn word_split_modifier() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "-w *.tmp *.bak *.log\n").expect("write");

        let rules = read_rules(&path).expect("read rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
        assert_eq!(rules[2].pattern(), "*.log");
    }
}

// ============================================================================
// 11. Parse Error Handling
// ============================================================================

mod parse_error_handling {
    use super::*;

    /// Test: Invalid rule returns error with line number.
    #[test]
    fn invalid_rule_error_with_line_number() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "- *.tmp\ninvalid rule\n- *.bak\n").expect("write");

        let result = read_rules(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.line, Some(2));
    }

    /// Test: Error includes file path.
    #[test]
    fn error_includes_file_path() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("bad_rules.txt");
        fs::write(&path, "garbage").expect("write");

        let result = read_rules(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.path.contains("bad_rules.txt"));
    }

    /// Test: Empty pattern after action.
    #[test]
    fn empty_pattern_error() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("excludes.txt");
        fs::write(&path, "+ \n").expect("write");

        let result = read_rules(&path);
        assert!(result.is_err());
    }
}

// ============================================================================
// 12. Integration Tests (End-to-End FilterSet)
// ============================================================================

mod integration_tests {
    use super::*;

    /// Test: Full workflow - read file, compile set, match paths.
    #[test]
    fn full_workflow() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("filters.txt");
        fs::write(
            &path,
            "# Project filters\n\
             + src/**\n\
             + Cargo.toml\n\
             - target/\n\
             - *.tmp\n\
             - *.log\n",
        )
        .expect("write");

        let rules = read_rules(&path).expect("read rules");
        let set = FilterSet::from_rules(rules).expect("compile");

        // Source files included
        assert!(set.allows(Path::new("src/main.rs"), false));
        assert!(set.allows(Path::new("src/lib/util.rs"), false));
        assert!(set.allows(Path::new("Cargo.toml"), false));

        // Target excluded
        assert!(!set.allows(Path::new("target"), true));
        assert!(!set.allows(Path::new("target/debug/app"), false));

        // Temp files excluded
        assert!(!set.allows(Path::new("scratch.tmp"), false));
        assert!(!set.allows(Path::new("debug.log"), false));
    }

    /// Test: Complex project structure filtering.
    /// Note: rsync uses first-match-wins, so we need to order rules properly.
    #[test]
    fn complex_project_filtering() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("filters.txt");
        fs::write(
            &path,
            "# Include only production code\n\
             # Test code exclusions must come BEFORE general includes (first-match-wins)\n\
             - src/**/test/**\n\
             - lib/**/test/**\n\
             + src/**\n\
             + lib/**\n\
             + Cargo.toml\n\
             + Cargo.lock\n\
             P Cargo.lock\n\
             - *\n",
        )
        .expect("write");

        let rules = read_rules(&path).expect("read rules");
        let set = FilterSet::from_rules(rules).expect("compile");

        // Production code included
        assert!(set.allows(Path::new("src/main.rs"), false));
        assert!(set.allows(Path::new("lib/core/mod.rs"), false));

        // Test code excluded (exclusion comes first in rules)
        assert!(!set.allows(Path::new("src/module/test/unit.rs"), false));

        // Config files included
        assert!(set.allows(Path::new("Cargo.toml"), false));
        assert!(set.allows(Path::new("Cargo.lock"), false));

        // Cargo.lock protected from deletion
        assert!(!set.allows_deletion(Path::new("Cargo.lock"), false));

        // Other files excluded
        assert!(!set.allows(Path::new("README.md"), false));
    }

    /// Test: CVS-style exclusion from file.
    #[test]
    fn cvs_style_exclusion() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("cvsignore.txt");
        fs::write(
            &path,
            "# CVS-style ignores\n\
             - RCS\n\
             - SCCS\n\
             - CVS\n\
             - CVS.adm\n\
             - RCSLOG\n\
             - *.o\n\
             - *.a\n\
             - *.so\n\
             - core\n\
             - *.swp\n\
             - *~\n",
        )
        .expect("write");

        let rules = read_rules(&path).expect("read rules");
        let set = FilterSet::from_rules(rules).expect("compile");

        assert!(!set.allows(Path::new("RCS"), false));
        assert!(!set.allows(Path::new("main.o"), false));
        assert!(!set.allows(Path::new("libfoo.a"), false));
        assert!(!set.allows(Path::new("libbar.so"), false));
        assert!(!set.allows(Path::new("core"), false));
        assert!(!set.allows(Path::new("file.swp"), false));
        assert!(!set.allows(Path::new("file~"), false));

        // Normal files allowed
        assert!(set.allows(Path::new("main.c"), false));
        assert!(set.allows(Path::new("header.h"), false));
    }
}
