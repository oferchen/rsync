//! Error, timeout, and rejection surface: I/O error categorization,
//! failed-directory propagation, legacy goodbye exchange, input-multiplex
//! activation, daemon filter rules, path-traversal rejection, and
//! sanitize-file-list trust gating.
//!
//! Split into per-concern submodules to keep each file focused and within
//! the 650-line cap:
//!
//! - [`legacy_goodbye_tests`] - protocol 28/29 NDX_DONE goodbye exchange.
//! - [`goodbye_partial_cutoff`] - EDG-GOODBYE.4 receiver-side EOF /
//!   garbage / hung-sender handling for the goodbye phase.
//! - [`goodbye_timeout_tests`] - EDG-GOODBYE.4 receiver-side timeout +
//!   disconnect surface for the goodbye phase. Simulates socket
//!   `set_read_timeout` firing mid-frame and asserts the receiver
//!   surfaces a typed `TimedOut` / `UnexpectedEof` within the
//!   configured window instead of blocking or silently succeeding.
//! - [`finalize_flush_tests`] - UTS-REVDD post-goodbye flush in
//!   `finalize_transfer` guarding against the upstream
//!   `reverse-daemon-delta` hang.
//! - [`input_multiplex_tests`] - client/server input multiplex activation
//!   per protocol version.
//! - [`sanitize_file_list`] - trust gating that strips absolute / `..`
//!   paths from an untrusted sender's file list.
//! - [`daemon_filter_tests`] - daemon-side `FilterSet` rules applied
//!   from `daemon_filter_rules`.

mod daemon_filter_tests;
mod finalize_flush_tests;
mod goodbye_partial_cutoff;
mod goodbye_timeout_tests;
mod input_multiplex_tests;
mod legacy_goodbye_tests;
mod sanitize_file_list;

use std::io;
use std::path::PathBuf;

use super::super::directory::FailedDirectories;
use super::super::stats::TransferStats;
use crate::error::{
    DeltaFatalError, DeltaRecoverableError, DeltaTransferError, categorize_io_error,
};

#[test]
fn error_categorization_disk_full_is_fatal() {
    use std::path::Path;

    let err = io::Error::from(io::ErrorKind::StorageFull);
    let path = Path::new("/tmp/test.txt");

    let categorized = categorize_io_error(err, path, "write");

    match categorized {
        DeltaTransferError::Fatal(DeltaFatalError::DiskFull { path: p, .. }) => {
            assert_eq!(p, path);
        }
        _ => panic!("Expected fatal disk full error"),
    }
}

#[test]
fn error_categorization_permission_denied_is_recoverable() {
    use std::path::Path;

    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/tmp/test.txt");

    let categorized = categorize_io_error(err, path, "open");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            path: p,
            operation: op,
        }) => {
            assert_eq!(p, path);
            assert_eq!(op, "open");
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

#[test]
fn error_categorization_not_found_is_recoverable() {
    use std::path::Path;

    let err = io::Error::from(io::ErrorKind::NotFound);
    let path = Path::new("/tmp/test.txt");

    let categorized = categorize_io_error(err, path, "open");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::FileNotFound { path: p }) => {
            assert_eq!(p, path);
        }
        _ => panic!("Expected recoverable file not found error"),
    }
}

#[test]
fn transfer_stats_tracks_metadata_errors() {
    let mut stats = TransferStats::default();

    assert_eq!(stats.metadata_errors.len(), 0);

    stats.metadata_errors.push((
        PathBuf::from("/tmp/file1.txt"),
        "Permission denied".to_owned(),
    ));
    stats.metadata_errors.push((
        PathBuf::from("/tmp/file2.txt"),
        "Operation not permitted".to_owned(),
    ));

    assert_eq!(stats.metadata_errors.len(), 2);
    assert_eq!(stats.metadata_errors[0].0, PathBuf::from("/tmp/file1.txt"));
    assert_eq!(stats.metadata_errors[0].1, "Permission denied");
}

#[test]
fn path_contains_dot_dot_simple_traversal() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "../etc/passwd"
    )));
}

#[test]
fn path_contains_dot_dot_mid_path() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "a/b/../../../etc/passwd"
    )));
}

#[test]
fn path_contains_dot_dot_trailing() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "a/b/.."
    )));
}

#[test]
fn path_contains_dot_dot_clean_path() {
    use std::path::Path;
    assert!(!super::super::quick_check::path_contains_dot_dot(
        Path::new("a/b/c")
    ));
}

#[test]
fn path_contains_dot_dot_dot_only() {
    use std::path::Path;
    // Single "." is not ".."
    assert!(!super::super::quick_check::path_contains_dot_dot(
        Path::new(".")
    ));
}

#[test]
fn path_contains_dot_dot_embedded_dots_in_name() {
    use std::path::Path;
    // "..." is not ".." - it's a normal filename
    assert!(!super::super::quick_check::path_contains_dot_dot(
        Path::new("a/.../b")
    ));
}

#[test]
fn path_contains_dot_dot_double_dotdot() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "a/../../b"
    )));
}

mod failed_directories_tests {
    use super::FailedDirectories;

    #[test]
    fn failed_directories_empty_has_no_ancestors() {
        let failed = FailedDirectories::new();
        assert!(failed.failed_ancestor("any/path/file.txt").is_none());
    }

    #[test]
    fn failed_directories_marks_and_finds_exact() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert!(failed.failed_ancestor("foo/bar").is_some());
    }

    #[test]
    fn failed_directories_finds_child_of_failed() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert_eq!(
            failed.failed_ancestor("foo/bar/baz/file.txt"),
            Some("foo/bar")
        );
    }

    #[test]
    fn failed_directories_does_not_match_sibling() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert!(failed.failed_ancestor("foo/other/file.txt").is_none());
    }

    #[test]
    fn failed_directories_counts_failures() {
        let mut failed = FailedDirectories::new();
        assert_eq!(failed.count(), 0);
        failed.mark_failed("a");
        failed.mark_failed("b");
        assert_eq!(failed.count(), 2);
    }

    // The seven cases below were the only assertions in a detached, never-compiled
    // copy of this type that this module did not already cover. The copy is gone;
    // these are kept because they pin real behaviour of the live implementation.

    #[test]
    fn failed_directories_does_not_match_parent() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");

        // Marking a child must not make its ancestors look failed.
        assert!(failed.failed_ancestor("foo/file.txt").is_none());
        assert!(failed.failed_ancestor("foo").is_none());
    }

    #[test]
    fn failed_directories_prefix_is_not_confused_by_similar_names() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("abc");

        // `abcd` shares a string prefix with `abc` but is not beneath it. The
        // walk truncates at `/`, so this must not match.
        assert!(failed.failed_ancestor("abcd").is_none());
        assert!(failed.failed_ancestor("abcd/file.txt").is_none());
        assert!(failed.failed_ancestor("abc/file.txt").is_some());
    }

    #[test]
    fn failed_directories_returns_the_closest_failed_ancestor() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a");
        failed.mark_failed("a/b");
        failed.mark_failed("a/b/c");

        // Nested marks all match; the walk reports the nearest one.
        assert_eq!(failed.failed_ancestor("a/b/c/d/file.txt"), Some("a/b/c"));
    }

    #[test]
    fn failed_directories_duplicate_marks_do_not_increase_count() {
        let mut failed = FailedDirectories::new();

        failed.mark_failed("foo/bar");
        assert_eq!(failed.count(), 1);
        failed.mark_failed("foo/bar");
        assert_eq!(failed.count(), 1);
    }

    #[test]
    fn failed_directories_entries_are_independent() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a/b");
        failed.mark_failed("x/y");

        assert_eq!(failed.failed_ancestor("a/b/c/file.txt"), Some("a/b"));
        assert_eq!(failed.failed_ancestor("x/y/z/file.txt"), Some("x/y"));
        assert!(failed.failed_ancestor("a/c/file.txt").is_none());
        assert!(failed.failed_ancestor("x/z/file.txt").is_none());
    }

    #[test]
    fn failed_directories_empty_path_matches_nothing() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo");

        assert!(failed.failed_ancestor("").is_none());
    }

    #[test]
    fn failed_directories_handles_a_path_without_separators() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("file");

        assert_eq!(failed.failed_ancestor("file"), Some("file"));
        assert!(failed.failed_ancestor("other").is_none());
    }
}
