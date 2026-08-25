//! Normalisation of the `--partial-dir` value, shared by the client argument
//! parser and the `--server` argument decoder.
//!
//! Upstream applies one rule to `partial_dir` at the end of option parsing,
//! inside `parse_arguments()`, which both the client and the server run. The
//! two roles therefore cannot legitimately disagree about what a given spelling
//! means. Keeping the rule in one place is what makes that guarantee explicit
//! rather than a coincidence of two copies staying in step.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/options.c:2594-2598` - the `if (partial_dir)` block, which
//!   sits OUTSIDE the `!am_server` guard at `:2590` that wraps the
//!   `RSYNC_PARTIAL_DIR` environment fallback.

use std::path::{Path, PathBuf};

/// Applies upstream's end-of-parse `--partial-dir` normalisation, returning
/// `None` for the spellings that mean "no partial directory".
///
/// Two rules, in upstream's order:
///
/// 1. a non-empty value is collapsed with `clean_fname(..,
///    CFN_COLLAPSE_DOT_DOT_DIRS)`, so `a/b/../c` names `a/c`;
/// 2. a value that is empty or exactly `.` becomes no partial directory at all.
///
/// Rule 2 has to run *after* rule 1, because the collapse is what turns `./`
/// into `.`. Both are delegated to [`filters::collapse_dot_dot_dirs`], oc's
/// port of `clean_fname`, which already yields `.` for an empty input - so the
/// two upstream arms fold into one comparison here without changing either.
///
/// Dropping the value matters beyond tidiness: `partial_dir_fname()` re-anchors
/// a relative partial directory at the destination file's own directory, so a
/// surviving `.` would make the staging target `<dirname>/./<basename>` - the
/// destination file itself, defeating the staging that `--delay-updates` and
/// `--partial-dir` exist to provide.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/options.c:2594-2598` `parse_arguments()`.
/// - `rsync-3.5.0/util1.c` `clean_fname()` with `CFN_COLLAPSE_DOT_DOT_DIRS`.
pub(crate) fn normalize_partial_dir(dir: &Path) -> Option<PathBuf> {
    let cleaned = filters::collapse_dot_dot_dirs(dir);
    (cleaned != Path::new(".")).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::normalize_partial_dir;
    use std::path::{Path, PathBuf};

    /// Every spelling upstream maps to "no partial directory" must yield
    /// `None`, and every other spelling must survive collapsed.
    ///
    /// The expectations are upstream's two arms read directly:
    /// `if (*partial_dir) clean_fname(...)` then
    /// `if (!*partial_dir || strcmp(partial_dir, ".") == 0) partial_dir = NULL`
    /// (options.c:2594-2598).
    #[test]
    fn matches_upstream_end_of_parse_normalisation() {
        for unset in ["", ".", "./", "./.", "a/.."] {
            assert_eq!(
                normalize_partial_dir(Path::new(unset)),
                None,
                "{unset:?} means no partial directory upstream"
            );
        }

        for (given, want) in [
            ("pdir", "pdir"),
            ("pdir/", "pdir"),
            ("./pdir", "pdir"),
            ("a/b/../c", "a/c"),
            ("a//b", "a/b"),
            // A `..` with nothing to consume is KEPT, so a value that climbs
            // above its anchor still says so rather than silently flattening.
            ("../pdir", "../pdir"),
            ("/abs/pdir", "/abs/pdir"),
        ] {
            assert_eq!(
                normalize_partial_dir(Path::new(given)),
                Some(PathBuf::from(want)),
                "{given:?} must collapse to {want:?}"
            );
        }
    }
}
