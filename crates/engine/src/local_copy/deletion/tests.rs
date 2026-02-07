//! Comprehensive tests for deletion strategy implementations.

use super::*;
use crate::local_copy::DeleteTiming;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Helper to create a deletion context with common defaults.
fn make_context(
    delete_enabled: bool,
    timing: DeleteTiming,
    max_deletions: Option<u64>,
) -> DeletionContext<'static> {
    DeletionContext::new(
        delete_enabled,
        timing,
        false,
        max_deletions,
        0,
        false,
        false,
        None,
    )
}

mod deletion_context_tests {
    use super::*;

    #[test]
    fn new_creates_context_with_correct_fields() {
        let ctx = DeletionContext::new(
            true,
            DeleteTiming::Before,
            true,
            Some(50),
            10,
            true,
            false,
            Some(Path::new("test")),
        );
        assert!(ctx.delete_enabled);
        assert_eq!(ctx.delete_timing, DeleteTiming::Before);
        assert!(ctx.delete_excluded);
        assert_eq!(ctx.max_deletions, Some(50));
        assert_eq!(ctx.deletions_performed, 10);
        assert!(ctx.force);
        assert!(!ctx.dry_run);
        assert_eq!(ctx.relative_path, Some(Path::new("test")));
    }

    #[test]
    fn can_delete_returns_true_when_no_limit() {
        let ctx = make_context(true, DeleteTiming::During, None);
        assert!(ctx.can_delete());
    }

    #[test]
    fn can_delete_returns_true_when_under_limit() {
        let mut ctx = make_context(true, DeleteTiming::During, Some(100));
        ctx.deletions_performed = 50;
        assert!(ctx.can_delete());
    }

    #[test]
    fn can_delete_returns_false_when_at_limit() {
        let mut ctx = make_context(true, DeleteTiming::During, Some(100));
        ctx.deletions_performed = 100;
        assert!(!ctx.can_delete());
    }

    #[test]
    fn can_delete_returns_false_when_over_limit() {
        let mut ctx = make_context(true, DeleteTiming::During, Some(100));
        ctx.deletions_performed = 150;
        assert!(!ctx.can_delete());
    }

    #[test]
    fn record_deletion_increments_counter() {
        let mut ctx = make_context(true, DeleteTiming::During, None);
        assert_eq!(ctx.deletions_performed, 0);
        ctx.record_deletion();
        assert_eq!(ctx.deletions_performed, 1);
        ctx.record_deletion();
        assert_eq!(ctx.deletions_performed, 2);
    }

    #[test]
    fn record_deletion_saturates_at_max() {
        let mut ctx = make_context(true, DeleteTiming::During, None);
        ctx.deletions_performed = u64::MAX;
        ctx.record_deletion();
        assert_eq!(ctx.deletions_performed, u64::MAX);
    }
}

mod strategy_tests {
    use super::*;
    use crate::local_copy::deletion::strategy::{
        DeleteAfterStrategy, DeleteBeforeStrategy, DeleteDelayStrategy, DeleteDuringStrategy,
        DeletionStrategy, apply_deletion_strategy,
    };

    #[test]
    fn delete_before_strategy_has_correct_timing() {
        let strategy = DeleteBeforeStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::Before);
    }

    #[test]
    fn delete_before_strategy_applies_immediately() {
        let strategy = DeleteBeforeStrategy;
        assert!(strategy.should_apply_immediately());
        assert!(!strategy.should_defer());
    }

    #[test]
    fn delete_during_strategy_has_correct_timing() {
        let strategy = DeleteDuringStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::During);
    }

    #[test]
    fn delete_during_strategy_applies_immediately() {
        let strategy = DeleteDuringStrategy;
        assert!(strategy.should_apply_immediately());
        assert!(!strategy.should_defer());
    }

    #[test]
    fn delete_after_strategy_has_correct_timing() {
        let strategy = DeleteAfterStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::After);
    }

    #[test]
    fn delete_after_strategy_defers_deletion() {
        let strategy = DeleteAfterStrategy;
        assert!(!strategy.should_apply_immediately());
        assert!(strategy.should_defer());
    }

    #[test]
    fn delete_delay_strategy_has_correct_timing() {
        let strategy = DeleteDelayStrategy;
        assert_eq!(strategy.timing(), DeleteTiming::Delay);
    }

    #[test]
    fn delete_delay_strategy_defers_deletion() {
        let strategy = DeleteDelayStrategy;
        assert!(!strategy.should_apply_immediately());
        assert!(strategy.should_defer());
    }

    #[test]
    fn apply_deletion_strategy_returns_correct_strategy_for_before() {
        let strategy = apply_deletion_strategy(DeleteTiming::Before);
        assert_eq!(strategy.timing(), DeleteTiming::Before);
        assert!(strategy.should_apply_immediately());
    }

    #[test]
    fn apply_deletion_strategy_returns_correct_strategy_for_during() {
        let strategy = apply_deletion_strategy(DeleteTiming::During);
        assert_eq!(strategy.timing(), DeleteTiming::During);
        assert!(strategy.should_apply_immediately());
    }

    #[test]
    fn apply_deletion_strategy_returns_correct_strategy_for_after() {
        let strategy = apply_deletion_strategy(DeleteTiming::After);
        assert_eq!(strategy.timing(), DeleteTiming::After);
        assert!(strategy.should_defer());
    }

    #[test]
    fn apply_deletion_strategy_returns_correct_strategy_for_delay() {
        let strategy = apply_deletion_strategy(DeleteTiming::Delay);
        assert_eq!(strategy.timing(), DeleteTiming::Delay);
        assert!(strategy.should_defer());
    }
}

mod is_extraneous_entry_tests {
    use super::*;
    use crate::local_copy::deletion::is_extraneous_entry;

    #[test]
    fn returns_true_for_entry_not_in_source() {
        let source = vec![OsString::from("keep1.txt"), OsString::from("keep2.txt")];
        assert!(is_extraneous_entry(OsStr::new("delete_me.txt"), &source));
    }

    #[test]
    fn returns_false_for_entry_in_source() {
        let source = vec![OsString::from("keep1.txt"), OsString::from("keep2.txt")];
        assert!(!is_extraneous_entry(OsStr::new("keep1.txt"), &source));
        assert!(!is_extraneous_entry(OsStr::new("keep2.txt"), &source));
    }

    #[test]
    fn returns_true_for_empty_source_list() {
        let source: Vec<OsString> = vec![];
        assert!(is_extraneous_entry(OsStr::new("anything.txt"), &source));
    }

    #[test]
    fn case_sensitive_matching() {
        let source = vec![OsString::from("File.txt")];
        assert!(!is_extraneous_entry(OsStr::new("File.txt"), &source));
        // On case-sensitive filesystems, different case = different file
        #[cfg(unix)]
        assert!(is_extraneous_entry(OsStr::new("file.txt"), &source));
    }

    #[test]
    fn handles_special_characters() {
        let source = vec![
            OsString::from("file with spaces.txt"),
            OsString::from("file-with-dashes.txt"),
            OsString::from("file_with_underscores.txt"),
        ];
        assert!(!is_extraneous_entry(
            OsStr::new("file with spaces.txt"),
            &source
        ));
        assert!(is_extraneous_entry(OsStr::new("other file.txt"), &source));
    }

    #[test]
    fn handles_unicode_filenames() {
        let source = vec![
            OsString::from("файл.txt"),     // Russian
            OsString::from("文件.txt"),     // Chinese
            OsString::from("ファイル.txt"), // Japanese
        ];
        assert!(!is_extraneous_entry(OsStr::new("файл.txt"), &source));
        assert!(!is_extraneous_entry(OsStr::new("文件.txt"), &source));
        assert!(is_extraneous_entry(OsStr::new("other.txt"), &source));
    }
}

mod should_delete_entry_tests {
    use super::*;
    use crate::local_copy::deletion::should_delete_entry;

    #[test]
    fn returns_false_when_deletion_disabled() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = make_context(false, DeleteTiming::During, None);
        assert!(!should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn returns_false_when_at_deletion_limit() {
        let source = vec![OsString::from("keep.txt")];
        let mut ctx = make_context(true, DeleteTiming::During, Some(10));
        ctx.deletions_performed = 10;
        assert!(!should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn returns_false_when_entry_is_in_source() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = make_context(true, DeleteTiming::During, None);
        assert!(!should_delete_entry(
            OsStr::new("keep.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn returns_false_when_filter_disallows_deletion() {
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
    fn returns_true_when_all_conditions_met() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = make_context(true, DeleteTiming::During, None);
        assert!(should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn returns_true_when_under_deletion_limit() {
        let source = vec![OsString::from("keep.txt")];
        let mut ctx = make_context(true, DeleteTiming::During, Some(10));
        ctx.deletions_performed = 5;
        assert!(should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| true
        ));
    }

    #[test]
    fn returns_true_with_no_relative_path() {
        let source = vec![OsString::from("keep.txt")];
        let ctx = make_context(true, DeleteTiming::During, None);
        assert!(should_delete_entry(
            OsStr::new("extra.txt"),
            &source,
            &ctx,
            |_, _| panic!("Filter should not be called without relative path")
        ));
    }
}

mod build_keep_set_tests {
    use super::*;
    use crate::local_copy::deletion::strategy::build_keep_set;

    #[test]
    fn creates_set_from_osstrings() {
        let entries = vec![
            OsString::from("file1.txt"),
            OsString::from("file2.txt"),
            OsString::from("dir"),
        ];
        let set = build_keep_set(&entries);
        assert_eq!(set.len(), 3);
        assert!(set.contains(OsStr::new("file1.txt")));
        assert!(set.contains(OsStr::new("file2.txt")));
        assert!(set.contains(OsStr::new("dir")));
    }

    #[test]
    fn creates_empty_set_from_empty_input() {
        let entries: Vec<OsString> = vec![];
        let set = build_keep_set(&entries);
        assert!(set.is_empty());
    }

    #[test]
    fn handles_duplicate_entries() {
        let entries = vec![
            OsString::from("file.txt"),
            OsString::from("file.txt"),
            OsString::from("other.txt"),
        ];
        let set = build_keep_set(&entries);
        // Set deduplicates
        assert_eq!(set.len(), 2);
        assert!(set.contains(OsStr::new("file.txt")));
        assert!(set.contains(OsStr::new("other.txt")));
    }

    #[test]
    fn works_with_string_references() {
        let entries = [OsString::from("a.txt"), OsString::from("b.txt")];
        let refs: Vec<&OsString> = entries.iter().collect();
        let set = build_keep_set(refs);
        assert_eq!(set.len(), 2);
        assert!(set.contains(OsStr::new("a.txt")));
    }
}

mod deletion_error_tests {
    use super::*;

    #[test]
    fn limit_exceeded_error_displays_correctly() {
        let err = DeletionError::LimitExceeded {
            attempted: 150,
            limit: 100,
        };
        let msg = format!("{err}");
        assert!(msg.contains("150"));
        assert!(msg.contains("100"));
        assert!(msg.contains("exceeded"));
    }

    #[test]
    fn limit_exceeded_has_no_source() {
        let err = DeletionError::LimitExceeded {
            attempted: 150,
            limit: 100,
        };
        assert!(err.source().is_none());
    }

    #[test]
    fn io_error_has_source() {
        use crate::local_copy::LocalCopyError;

        let io_err = LocalCopyError::io(
            "test",
            PathBuf::from("/test"),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        let err = DeletionError::Io(io_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn from_local_copy_error_creates_io_variant() {
        use crate::local_copy::LocalCopyError;

        let local_err = LocalCopyError::io(
            "test",
            PathBuf::from("/test"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        let deletion_err = DeletionError::from(local_err);
        assert!(matches!(deletion_err, DeletionError::Io(_)));
    }
}

mod integration_tests {
    use super::*;
    use crate::local_copy::deletion::{apply_deletion_strategy, should_delete_entry};

    /// Simulates a complete deletion workflow for delete-before timing.
    #[test]
    fn delete_before_workflow() {
        let source_entries = vec![OsString::from("keep1.txt"), OsString::from("keep2.txt")];
        let dest_entries = vec![
            OsString::from("keep1.txt"),
            OsString::from("keep2.txt"),
            OsString::from("delete1.txt"),
            OsString::from("delete2.txt"),
        ];

        let strategy = apply_deletion_strategy(DeleteTiming::Before);
        assert!(strategy.should_apply_immediately());

        let ctx = make_context(true, DeleteTiming::Before, None);

        let mut to_delete = Vec::new();
        for entry in &dest_entries {
            if should_delete_entry(entry, &source_entries, &ctx, |_, _| true) {
                to_delete.push(entry.clone());
            }
        }

        assert_eq!(to_delete.len(), 2);
        assert!(to_delete.contains(&OsString::from("delete1.txt")));
        assert!(to_delete.contains(&OsString::from("delete2.txt")));
    }

    /// Simulates a complete deletion workflow for delete-after timing.
    #[test]
    fn delete_after_workflow() {
        let source_entries = vec![OsString::from("keep.txt")];
        let dest_entries = vec![OsString::from("keep.txt"), OsString::from("delete.txt")];

        let strategy = apply_deletion_strategy(DeleteTiming::After);
        assert!(strategy.should_defer());

        let ctx = make_context(true, DeleteTiming::After, None);

        // During transfer, we build up the deletion list but don't apply
        let mut deferred = Vec::new();
        for entry in &dest_entries {
            if should_delete_entry(entry, &source_entries, &ctx, |_, _| true) {
                deferred.push(entry.clone());
            }
        }

        // After transfer, we apply deletions
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0], OsString::from("delete.txt"));
    }

    /// Simulates max-delete limit enforcement.
    #[test]
    fn max_delete_limit_workflow() {
        let source_entries: Vec<OsString> = vec![];
        let dest_entries = vec![
            OsString::from("delete1.txt"),
            OsString::from("delete2.txt"),
            OsString::from("delete3.txt"),
        ];

        let mut ctx = make_context(true, DeleteTiming::During, Some(2));

        let mut deleted_count = 0;
        for entry in &dest_entries {
            if should_delete_entry(entry, &source_entries, &ctx, |_, _| true) {
                deleted_count += 1;
                ctx.record_deletion();
            }
        }

        // Only 2 should be deleted due to limit
        assert_eq!(deleted_count, 2);
        assert_eq!(ctx.deletions_performed, 2);
    }
}
