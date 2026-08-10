//! Setter methods for deletion-related builder options.

use super::LocalCopyOptionsBuilder;
use crate::local_copy::options::types::DeleteTiming;

impl LocalCopyOptionsBuilder {
    /// Enables or disables deletion of extraneous destination files.
    #[must_use]
    pub fn delete(mut self, enabled: bool) -> Self {
        self.delete = enabled;
        if enabled {
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Sets the timing for deletion operations.
    #[must_use]
    pub fn delete_timing(mut self, timing: DeleteTiming) -> Self {
        self.delete_timing = timing;
        if !matches!(timing, DeleteTiming::During) {
            self.delete = true;
        }
        self
    }

    /// Enables deletion before transfer.
    #[must_use]
    pub fn delete_before(mut self, enabled: bool) -> Self {
        if enabled {
            self.delete = true;
            self.delete_timing = DeleteTiming::Before;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Before) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Enables deletion after transfer.
    #[must_use]
    pub fn delete_after(mut self, enabled: bool) -> Self {
        if enabled {
            self.delete = true;
            self.delete_timing = DeleteTiming::After;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::After) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Enables delayed deletion.
    #[must_use]
    pub fn delete_delay(mut self, enabled: bool) -> Self {
        if enabled {
            self.delete = true;
            self.delete_timing = DeleteTiming::Delay;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Delay) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Enables deletion during transfer.
    #[must_use]
    pub fn delete_during(mut self) -> Self {
        self.delete = true;
        self.delete_timing = DeleteTiming::During;
        self
    }

    /// Enables deletion of excluded files.
    #[must_use]
    pub fn delete_excluded(mut self, enabled: bool) -> Self {
        self.delete_excluded = enabled;
        self
    }

    /// Enables deletion of files corresponding to missing source arguments.
    #[must_use]
    pub fn delete_missing_args(mut self, enabled: bool) -> Self {
        self.delete_missing_args = enabled;
        self
    }

    /// Sets the maximum number of deletions allowed.
    #[must_use]
    pub fn max_deletions(mut self, limit: Option<u64>) -> Self {
        self.max_deletions = limit;
        self
    }
}
