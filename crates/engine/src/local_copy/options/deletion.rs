use super::types::{DeleteTiming, LocalCopyOptions};

impl LocalCopyOptions {
    /// Requests that destination files absent from the source be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete = delete;
        if delete {
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Requests that extraneous destination files be removed after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(mut self, delete_after: bool) -> Self {
        if delete_after {
            self.delete = true;
            self.delete_timing = DeleteTiming::After;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::After) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Queues deletions discovered during the walk and applies them after transfers finish.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(mut self, delete_delay: bool) -> Self {
        if delete_delay {
            self.delete = true;
            self.delete_timing = DeleteTiming::Delay;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Delay) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Requests that extraneous destination files be removed before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(mut self, delete_before: bool) -> Self {
        if delete_before {
            self.delete = true;
            self.delete_timing = DeleteTiming::Before;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Before) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Requests that extraneous destination files be removed while processing directories.
    #[must_use]
    #[doc(alias = "--delete-during")]
    pub const fn delete_during(mut self) -> Self {
        if self.delete {
            self.delete_timing = DeleteTiming::During;
        } else {
            self.delete = true;
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Requests that excluded destination entries be removed during deletion sweeps.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(mut self, delete: bool) -> Self {
        self.delete_excluded = delete;
        self
    }

    /// Limits the number of deletions performed during a transfer.
    #[must_use]
    #[doc(alias = "--max-delete")]
    pub const fn max_deletions(mut self, limit: Option<u64>) -> Self {
        self.max_deletions = limit;
        self
    }

    /// Reports whether extraneous destination files should be removed.
    #[must_use]
    pub const fn delete_extraneous(&self) -> bool {
        self.delete
    }

    /// Returns the configured maximum number of deletions, if any.
    #[must_use]
    pub const fn max_deletion_limit(&self) -> Option<u64> {
        self.max_deletions
    }

    /// Returns the configured deletion timing when deletion sweeps are enabled.
    #[must_use]
    pub const fn delete_timing(&self) -> Option<DeleteTiming> {
        if self.delete {
            Some(self.delete_timing)
        } else {
            None
        }
    }

    /// Reports whether deletions should occur before content transfers.
    #[must_use]
    pub const fn delete_before_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::Before) && self.delete
    }

    /// Reports whether deletions should occur after transfers instead of immediately.
    #[must_use]
    pub const fn delete_after_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::After) && self.delete
    }

    /// Reports whether deletions are deferred until after transfers but determined during the walk.
    #[must_use]
    pub const fn delete_delay_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::Delay) && self.delete
    }

    /// Reports whether deletions should occur while processing directory entries.
    #[must_use]
    pub const fn delete_during_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::During) && self.delete
    }

    /// Reports whether excluded paths should also be removed during deletion sweeps.
    #[must_use]
    pub const fn delete_excluded_enabled(&self) -> bool {
        self.delete_excluded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_enables_deletion() {
        let opts = LocalCopyOptions::new().delete(true);
        assert!(opts.delete_extraneous());
        assert_eq!(opts.delete_timing(), Some(DeleteTiming::During));
    }

    #[test]
    fn delete_false_disables_deletion() {
        let opts = LocalCopyOptions::new().delete(false);
        assert!(!opts.delete_extraneous());
    }

    #[test]
    fn delete_after_enables_with_after_timing() {
        let opts = LocalCopyOptions::new().delete_after(true);
        assert!(opts.delete_extraneous());
        assert_eq!(opts.delete_timing(), Some(DeleteTiming::After));
        assert!(opts.delete_after_enabled());
    }

    #[test]
    fn delete_after_false_disables_if_timing_is_after() {
        let opts = LocalCopyOptions::new()
            .delete_after(true)
            .delete_after(false);
        assert!(!opts.delete_extraneous());
        assert!(opts.delete_timing().is_none());
    }

    #[test]
    fn delete_before_enables_with_before_timing() {
        let opts = LocalCopyOptions::new().delete_before(true);
        assert!(opts.delete_extraneous());
        assert_eq!(opts.delete_timing(), Some(DeleteTiming::Before));
        assert!(opts.delete_before_enabled());
    }

    #[test]
    fn delete_before_false_disables_if_timing_is_before() {
        let opts = LocalCopyOptions::new()
            .delete_before(true)
            .delete_before(false);
        assert!(!opts.delete_extraneous());
    }

    #[test]
    fn delete_delay_enables_with_delay_timing() {
        let opts = LocalCopyOptions::new().delete_delay(true);
        assert!(opts.delete_extraneous());
        assert_eq!(opts.delete_timing(), Some(DeleteTiming::Delay));
        assert!(opts.delete_delay_enabled());
    }

    #[test]
    fn delete_delay_false_disables_if_timing_is_delay() {
        let opts = LocalCopyOptions::new()
            .delete_delay(true)
            .delete_delay(false);
        assert!(!opts.delete_extraneous());
    }

    #[test]
    fn delete_during_enables_with_during_timing() {
        let opts = LocalCopyOptions::new().delete_during();
        assert!(opts.delete_extraneous());
        assert_eq!(opts.delete_timing(), Some(DeleteTiming::During));
        assert!(opts.delete_during_enabled());
    }

    #[test]
    fn delete_during_changes_timing_if_already_enabled() {
        let opts = LocalCopyOptions::new().delete_after(true).delete_during();
        assert!(opts.delete_extraneous());
        assert_eq!(opts.delete_timing(), Some(DeleteTiming::During));
    }

    #[test]
    fn delete_excluded_enables_exclusion_deletion() {
        let opts = LocalCopyOptions::new().delete_excluded(true);
        assert!(opts.delete_excluded_enabled());
    }

    #[test]
    fn delete_excluded_false_disables() {
        let opts = LocalCopyOptions::new()
            .delete_excluded(true)
            .delete_excluded(false);
        assert!(!opts.delete_excluded_enabled());
    }

    #[test]
    fn max_deletions_sets_limit() {
        let opts = LocalCopyOptions::new().max_deletions(Some(100));
        assert_eq!(opts.max_deletion_limit(), Some(100));
    }

    #[test]
    fn max_deletions_none_clears_limit() {
        let opts = LocalCopyOptions::new()
            .max_deletions(Some(100))
            .max_deletions(None);
        assert!(opts.max_deletion_limit().is_none());
    }

    #[test]
    fn delete_timing_returns_none_when_delete_disabled() {
        let opts = LocalCopyOptions::new();
        assert!(opts.delete_timing().is_none());
    }

    #[test]
    fn delete_before_enabled_returns_false_when_delete_disabled() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.delete_before_enabled());
    }

    #[test]
    fn delete_after_enabled_returns_false_when_delete_disabled() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.delete_after_enabled());
    }

    #[test]
    fn delete_delay_enabled_returns_false_when_delete_disabled() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.delete_delay_enabled());
    }

    #[test]
    fn delete_during_enabled_returns_false_when_delete_disabled() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.delete_during_enabled());
    }

    #[test]
    fn delete_timing_default_is_during() {
        assert_eq!(DeleteTiming::default(), DeleteTiming::During);
    }
}
