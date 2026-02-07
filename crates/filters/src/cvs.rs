//! CVS exclusion patterns for rsync's `--cvs-exclude` (`-C`) option.
//!
//! This module provides the default exclusion patterns that rsync uses when
//! the `--cvs-exclude` option is specified. These patterns automatically
//! exclude files commonly found in version control systems and build artifacts.
//!
//! # Default Patterns
//!
//! The default CVS exclusion list includes:
//!
//! - Version control directories: `RCS`, `SCCS`, `CVS`, `.svn/`, `.git/`, `.hg/`, `.bzr/`
//! - Version control files: `CVS.adm`, `RCSLOG`, `cvslog.*`, `tags`, `TAGS`
//! - Build state files: `.make.state`, `.nse_depinfo`
//! - Editor backup files: `*~`, `#*`, `.#*`, `*.old`, `*.bak`, `*.BAK`, `*.orig`, `*.rej`
//! - Temporary files: `,*`, `_$*`, `*$`, `.del-*`
//! - Object files and libraries: `*.a`, `*.olb`, `*.o`, `*.obj`, `*.so`, `*.exe`
//! - Miscellaneous: `*.Z`, `*.elc`, `*.ln`, `core`
//!
//! # Perishable Rules
//!
//! In rsync protocol version 30 and later, these patterns are marked as
//! "perishable", meaning they can be overridden by explicit include rules
//! from `.cvsignore` files or the `CVSIGNORE` environment variable.
//!
//! # References
//!
//! - [rsync man page](https://download.samba.org/pub/rsync/rsync.1)
//! - Upstream rsync `exclude.c` and `options.c`

/// Default CVS exclusion patterns.
///
/// These patterns are space-separated and match rsync's built-in defaults.
/// The patterns use rsync's filter syntax:
/// - Simple names match files/directories at any level
/// - `*` matches any characters except `/`
/// - Trailing `/` marks directory-only patterns
pub const DEFAULT_CVSIGNORE: &str = concat!(
    // Version control systems
    "RCS ",
    "SCCS ",
    "CVS ",
    "CVS.adm ",
    "RCSLOG ",
    "cvslog.* ",
    "tags ",
    "TAGS ",
    // Build state
    ".make.state ",
    ".nse_depinfo ",
    // Editor backup files
    "*~ ",
    "#* ",
    ".#* ",
    ",* ",
    "_$* ",
    "*$ ",
    // Backup/original files
    "*.old ",
    "*.bak ",
    "*.BAK ",
    "*.orig ",
    "*.rej ",
    ".del-* ",
    // Object files and libraries
    "*.a ",
    "*.olb ",
    "*.o ",
    "*.obj ",
    "*.so ",
    "*.exe ",
    // Miscellaneous
    "*.Z ",
    "*.elc ",
    "*.ln ",
    "core ",
    // Modern VCS directories
    ".svn/ ",
    ".git/ ",
    ".hg/ ",
    ".bzr/",
);

/// Returns an iterator over the default CVS exclusion patterns.
///
/// Each pattern is whitespace-trimmed and ready for use with filter rules.
///
/// # Examples
///
/// ```
/// use filters::cvs::default_patterns;
///
/// let patterns: Vec<&str> = default_patterns().collect();
/// assert!(patterns.contains(&"*.o"));
/// assert!(patterns.contains(&".git/"));
/// ```
pub fn default_patterns() -> impl Iterator<Item = &'static str> {
    DEFAULT_CVSIGNORE
        .split_whitespace()
        .filter(|s| !s.is_empty())
}

/// Returns the number of default CVS exclusion patterns in
/// [`DEFAULT_CVSIGNORE`].
pub fn pattern_count() -> usize {
    default_patterns().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_patterns_contains_expected_vcs_dirs() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&".git/"));
        assert!(patterns.contains(&".svn/"));
        assert!(patterns.contains(&".hg/"));
        assert!(patterns.contains(&".bzr/"));
        assert!(patterns.contains(&"CVS"));
        assert!(patterns.contains(&"RCS"));
        assert!(patterns.contains(&"SCCS"));
    }

    #[test]
    fn default_patterns_contains_object_files() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&"*.o"));
        assert!(patterns.contains(&"*.obj"));
        assert!(patterns.contains(&"*.so"));
        assert!(patterns.contains(&"*.a"));
        assert!(patterns.contains(&"*.exe"));
    }

    #[test]
    fn default_patterns_contains_backup_files() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&"*~"));
        assert!(patterns.contains(&"*.bak"));
        assert!(patterns.contains(&"*.BAK"));
        assert!(patterns.contains(&"*.old"));
        assert!(patterns.contains(&"*.orig"));
        assert!(patterns.contains(&"*.rej"));
    }

    #[test]
    fn default_patterns_contains_editor_files() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&"#*"));
        assert!(patterns.contains(&".#*"));
    }

    #[test]
    fn default_patterns_count_matches_expected() {
        // Based on upstream rsync's default list
        let count = pattern_count();
        assert!(count >= 30, "expected at least 30 patterns, got {count}");
        assert!(count <= 40, "expected at most 40 patterns, got {count}");
    }

    #[test]
    fn default_patterns_no_empty_entries() {
        for pattern in default_patterns() {
            assert!(!pattern.is_empty(), "found empty pattern");
            assert!(
                !pattern.contains(' '),
                "pattern contains space: {pattern:?}"
            );
        }
    }

    #[test]
    fn default_patterns_directory_markers_preserved() {
        let patterns: Vec<&str> = default_patterns().collect();
        // Directory patterns should end with /
        assert!(patterns.contains(&".git/"));
        assert!(patterns.contains(&".svn/"));
        // Non-directory patterns should not end with /
        assert!(patterns.contains(&"*.o"));
        assert!(!patterns.contains(&"*.o/"));
    }
}
