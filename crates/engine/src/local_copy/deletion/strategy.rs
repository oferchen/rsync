//! Core deletion strategy implementation.
//!
//! This module encapsulates the logic for determining which files should be
//! deleted and when, mirroring upstream rsync's deletion behavior.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::Path;

use crate::local_copy::{DeleteTiming, LocalCopyError};

/// Result type for deletion operations.
pub type DeletionResult<T> = Result<T, DeletionError>;

/// Errors that can occur during deletion operations.
#[derive(Debug)]
pub enum DeletionError {
    /// Maximum deletion limit exceeded.
    LimitExceeded {
        /// Number of deletions attempted when limit was hit.
        attempted: u64,
        /// The configured deletion limit.
        limit: u64,
    },
    /// I/O error during deletion.
    Io(LocalCopyError),
}

impl fmt::Display for DeletionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded { attempted, limit } => {
                write!(
                    f,
                    "deletion limit exceeded: attempted to delete {} files, limit is {}",
                    attempted, limit
                )
            }
            Self::Io(err) => write!(f, "I/O error during deletion: {}", err),
        }
    }
}

impl std::error::Error for DeletionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LimitExceeded { .. } => None,
            Self::Io(err) => Some(err),
        }
    }
}

impl From<LocalCopyError> for DeletionError {
    fn from(err: LocalCopyError) -> Self {
        Self::Io(err)
    }
}

/// Context information needed for deletion decisions.
///
/// This struct encapsulates all the state needed to determine whether
/// a file should be deleted, without requiring access to the full
/// `CopyContext`.
#[derive(Debug)]
pub struct DeletionContext<'a> {
    /// Whether deletion is enabled.
    pub delete_enabled: bool,
    /// When to perform deletions.
    pub delete_timing: DeleteTiming,
    /// Whether to delete excluded files.
    pub delete_excluded: bool,
    /// Maximum number of deletions allowed (None = unlimited).
    pub max_deletions: Option<u64>,
    /// Current number of deletions performed.
    pub deletions_performed: u64,
    /// Whether to force deletion of non-empty directories.
    pub force: bool,
    /// Whether this is a dry-run (report but don't delete).
    pub dry_run: bool,
    /// Path relative to transfer root (for filter evaluation).
    pub relative_path: Option<&'a Path>,
}

impl<'a> DeletionContext<'a> {
    /// Creates a new deletion context with the given settings.
    #[must_use]
    pub const fn new(
        delete_enabled: bool,
        delete_timing: DeleteTiming,
        delete_excluded: bool,
        max_deletions: Option<u64>,
        deletions_performed: u64,
        force: bool,
        dry_run: bool,
        relative_path: Option<&'a Path>,
    ) -> Self {
        Self {
            delete_enabled,
            delete_timing,
            delete_excluded,
            max_deletions,
            deletions_performed,
            force,
            dry_run,
            relative_path,
        }
    }

    /// Checks if more deletions are allowed under the max-delete limit.
    #[must_use]
    pub const fn can_delete(&self) -> bool {
        if let Some(limit) = self.max_deletions {
            self.deletions_performed < limit
        } else {
            true
        }
    }

    /// Records that a deletion was performed.
    pub fn record_deletion(&mut self) {
        self.deletions_performed = self.deletions_performed.saturating_add(1);
    }
}

/// Strategy for handling file deletions.
///
/// This trait defines the interface for different deletion timing strategies.
/// Each strategy determines when deletions should be applied during the
/// transfer process.
pub trait DeletionStrategy {
    /// Returns the timing for this strategy.
    fn timing(&self) -> DeleteTiming;

    /// Determines if deletions should be applied immediately.
    ///
    /// - `Before`: deletions happen before transfers start
    /// - `During`: deletions happen as directories are processed
    /// - `After`/`Delay`: deletions are deferred until after transfers
    fn should_apply_immediately(&self) -> bool {
        matches!(self.timing(), DeleteTiming::Before | DeleteTiming::During)
    }

    /// Determines if deletions should be deferred.
    fn should_defer(&self) -> bool {
        matches!(self.timing(), DeleteTiming::After | DeleteTiming::Delay)
    }
}

/// Strategy for --delete-before.
///
/// Removes extraneous files from the destination before any transfers begin.
/// This ensures maximum free space is available during the transfer.
#[derive(Debug, Clone, Copy)]
pub struct DeleteBeforeStrategy;

impl DeletionStrategy for DeleteBeforeStrategy {
    fn timing(&self) -> DeleteTiming {
        DeleteTiming::Before
    }
}

/// Strategy for --delete-during.
///
/// Removes extraneous files as each directory is processed. This is the
/// default and most memory-efficient approach.
#[derive(Debug, Clone, Copy)]
pub struct DeleteDuringStrategy;

impl DeletionStrategy for DeleteDuringStrategy {
    fn timing(&self) -> DeleteTiming {
        DeleteTiming::During
    }
}

/// Strategy for --delete-after.
///
/// Defers all deletions until after the transfer completes. This ensures
/// that files remain available during the transfer in case the transfer
/// is interrupted.
#[derive(Debug, Clone, Copy)]
pub struct DeleteAfterStrategy;

impl DeletionStrategy for DeleteAfterStrategy {
    fn timing(&self) -> DeleteTiming {
        DeleteTiming::After
    }
}

/// Strategy for --delete-delay.
///
/// Like --delete-after but with a separate deletion queue accumulated
/// during the walk. Useful with --delay-updates to ensure consistency.
#[derive(Debug, Clone, Copy)]
pub struct DeleteDelayStrategy;

impl DeletionStrategy for DeleteDelayStrategy {
    fn timing(&self) -> DeleteTiming {
        DeleteTiming::Delay
    }
}

/// Determines if a destination entry is extraneous (not in source).
///
/// An entry is extraneous if its name does not appear in the source
/// entry list. This is the core logic for identifying files to delete.
///
/// # Arguments
///
/// * `entry_name` - Name of the destination entry
/// * `source_entries` - Names of entries in the source directory
///
/// # Returns
///
/// `true` if the entry should be considered for deletion.
pub fn is_extraneous_entry<S: AsRef<OsStr>>(
    entry_name: &OsStr,
    source_entries: &[S],
) -> bool {
    !source_entries.iter().any(|s| s.as_ref() == entry_name)
}

/// Determines if an entry should be deleted based on context and filters.
///
/// This function encapsulates the deletion decision logic, considering:
/// - Whether deletion is enabled
/// - Filter rules and exclusion patterns
/// - The --delete-excluded flag
/// - The --max-delete limit
///
/// # Arguments
///
/// * `entry_name` - Name of the entry to check
/// * `source_entries` - Names of source entries to keep
/// * `context` - Deletion context with settings and limits
/// * `filter_allows_deletion` - Function to check if filters allow deletion
///
/// # Returns
///
/// `true` if the entry should be deleted.
pub fn should_delete_entry<S, F>(
    entry_name: &OsStr,
    source_entries: &[S],
    context: &DeletionContext<'_>,
    filter_allows_deletion: F,
) -> bool
where
    S: AsRef<OsStr>,
    F: FnOnce(&Path, bool) -> bool,
{
    // Check if deletion is enabled
    if !context.delete_enabled {
        return false;
    }

    // Check deletion limit
    if !context.can_delete() {
        return false;
    }

    // Check if entry is extraneous
    if !is_extraneous_entry(entry_name, source_entries) {
        return false;
    }

    // Check filter rules if we have a relative path
    if let Some(relative) = context.relative_path {
        // Filter evaluation determines if this file can be deleted
        // based on exclusion rules and --delete-excluded setting
        filter_allows_deletion(relative, false)
    } else {
        // No filter context, allow deletion
        true
    }
}

/// Applies the appropriate deletion strategy based on timing.
///
/// This is the main entry point for strategy-based deletion. It returns
/// the correct strategy implementation for the given timing.
///
/// # Arguments
///
/// * `timing` - The deletion timing mode
///
/// # Returns
///
/// A boxed deletion strategy implementation.
///
/// # Examples
///
/// ```ignore
/// let strategy = apply_deletion_strategy(DeleteTiming::Before);
/// if strategy.should_apply_immediately() {
///     // Perform deletions now
/// }
/// ```
pub fn apply_deletion_strategy(timing: DeleteTiming) -> Box<dyn DeletionStrategy> {
    match timing {
        DeleteTiming::Before => Box::new(DeleteBeforeStrategy),
        DeleteTiming::During => Box::new(DeleteDuringStrategy),
        DeleteTiming::After => Box::new(DeleteAfterStrategy),
        DeleteTiming::Delay => Box::new(DeleteDelayStrategy),
    }
}

/// Builds a set of entry names to keep (avoid deletion).
///
/// This helper function creates an efficient lookup structure for
/// checking if an entry should be preserved.
///
/// # Arguments
///
/// * `entries` - Iterator of entry names to keep
///
/// # Returns
///
/// A `HashSet` containing the entry names.
#[must_use]
pub fn build_keep_set<'a, I, S>(entries: I) -> HashSet<&'a OsStr>
where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<OsStr> + 'a,
{
    entries.into_iter().map(|s| s.as_ref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn deletion_context_can_delete_with_no_limit() {
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            None,
            10,
            false,
            false,
            None,
        );
        assert!(ctx.can_delete());
    }

    #[test]
    fn deletion_context_can_delete_under_limit() {
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            Some(100),
            50,
            false,
            false,
            None,
        );
        assert!(ctx.can_delete());
    }

    #[test]
    fn deletion_context_cannot_delete_at_limit() {
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            Some(100),
            100,
            false,
            false,
            None,
        );
        assert!(!ctx.can_delete());
    }

    #[test]
    fn deletion_context_cannot_delete_over_limit() {
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            Some(100),
            150,
            false,
            false,
            None,
        );
        assert!(!ctx.can_delete());
    }

    #[test]
    fn deletion_context_record_deletion_increments_count() {
        let mut ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            None,
            10,
            false,
            false,
            None,
        );
        ctx.record_deletion();
        assert_eq!(ctx.deletions_performed, 11);
    }

    #[test]
    fn deletion_context_record_deletion_saturates() {
        let mut ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            None,
            u64::MAX,
            false,
            false,
            None,
        );
        ctx.record_deletion();
        assert_eq!(ctx.deletions_performed, u64::MAX);
    }

    #[test]
    fn delete_before_strategy_timing() {
        let strategy = DeleteBeforeStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::Before);
        assert!(strategy.should_apply_immediately());
        assert!(!strategy.should_defer());
    }

    #[test]
    fn delete_during_strategy_timing() {
        let strategy = DeleteDuringStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::During);
        assert!(strategy.should_apply_immediately());
        assert!(!strategy.should_defer());
    }

    #[test]
    fn delete_after_strategy_timing() {
        let strategy = DeleteAfterStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::After);
        assert!(!strategy.should_apply_immediately());
        assert!(strategy.should_defer());
    }

    #[test]
    fn delete_delay_strategy_timing() {
        let strategy = DeleteDelayStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::Delay);
        assert!(!strategy.should_apply_immediately());
        assert!(strategy.should_defer());
    }

    #[test]
    fn apply_deletion_strategy_creates_correct_strategy() {
        let before = apply_deletion_strategy(DeleteTiming::Before);
        assert_eq!(before.timing(), DeleteTiming::Before);

        let during = apply_deletion_strategy(DeleteTiming::During);
        assert_eq!(during.timing(), DeleteTiming::During);

        let after = apply_deletion_strategy(DeleteTiming::After);
        assert_eq!(after.timing(), DeleteTiming::After);

        let delay = apply_deletion_strategy(DeleteTiming::Delay);
        assert_eq!(delay.timing(), DeleteTiming::Delay);
    }

    #[test]
    fn is_extraneous_entry_detects_missing_entry() {
        let source = vec![OsString::from("keep1.txt"), OsString::from("keep2.txt")];
        assert!(is_extraneous_entry(OsStr::new("extra.txt"), &source));
    }

    #[test]
    fn is_extraneous_entry_preserves_present_entry() {
        let source = vec![OsString::from("keep1.txt"), OsString::from("keep2.txt")];
        assert!(!is_extraneous_entry(OsStr::new("keep1.txt"), &source));
        assert!(!is_extraneous_entry(OsStr::new("keep2.txt"), &source));
    }

    #[test]
    fn is_extraneous_entry_empty_source_all_extraneous() {
        let source: Vec<OsString> = vec![];
        assert!(is_extraneous_entry(OsStr::new("anything.txt"), &source));
    }

    #[test]
    fn should_delete_entry_requires_deletion_enabled() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = DeletionContext::new(
            false,
            DeleteTiming::During,
            false,
            None,
            0,
            false,
            false,
            None,
        );
        assert!(!should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn should_delete_entry_respects_limit() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            Some(0),
            0,
            false,
            false,
            None,
        );
        assert!(!should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn should_delete_entry_requires_extraneous() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            None,
            0,
            false,
            false,
            None,
        );
        assert!(!should_delete_entry(
            OsStr::new("keep.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn should_delete_entry_respects_filter() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            None,
            0,
            false,
            false,
            Some(Path::new("extra.txt")),
        );
        assert!(!should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| false
        ));
    }

    #[test]
    fn should_delete_entry_deletes_extraneous_when_allowed() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::During,
            false,
            None,
            0,
            false,
            false,
            None,
        );
        assert!(should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn build_keep_set_creates_set_from_strings() {
        let entries = vec![OsString::from("a"), OsString::from("b"), OsString::from("c")];
        let set = build_keep_set(&entries);
        assert_eq!(set.len(), 3);
        assert!(set.contains(OsStr::new("a")));
        assert!(set.contains(OsStr::new("b")));
        assert!(set.contains(OsStr::new("c")));
    }

    #[test]
    fn build_keep_set_empty_input() {
        let entries: Vec<OsString> = vec![];
        let set = build_keep_set(&entries);
        assert!(set.is_empty());
    }

    #[test]
    fn deletion_error_display_limit_exceeded() {
        let err = DeletionError::LimitExceeded {
            attempted: 150,
            limit: 100,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("150"));
        assert!(msg.contains("100"));
        assert!(msg.contains("limit exceeded"));
    }
}
