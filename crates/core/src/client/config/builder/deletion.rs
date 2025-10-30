use super::*;

impl ClientConfigBuilder {
    /// Enables or disables deletion of extraneous destination files.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete_mode = if delete {
            DeleteMode::During
        } else {
            DeleteMode::Disabled
        };
        self
    }

    /// Requests deletion of extraneous entries before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(mut self, delete_before: bool) -> Self {
        if delete_before {
            self.delete_mode = DeleteMode::Before;
        } else if matches!(self.delete_mode, DeleteMode::Before) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Requests deletion of extraneous entries while directories are processed.
    #[must_use]
    #[doc(alias = "--delete-during")]
    pub const fn delete_during(mut self) -> Self {
        self.delete_mode = DeleteMode::During;
        self
    }

    /// Enables deletion of extraneous entries after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(mut self, delete_after: bool) -> Self {
        if delete_after {
            self.delete_mode = DeleteMode::After;
        } else if matches!(self.delete_mode, DeleteMode::After) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Requests delayed deletion sweeps that run after transfers complete.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(mut self, delete_delay: bool) -> Self {
        if delete_delay {
            self.delete_mode = DeleteMode::Delay;
        } else if matches!(self.delete_mode, DeleteMode::Delay) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Enables or disables deletion of excluded destination entries.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(mut self, delete: bool) -> Self {
        self.delete_excluded = delete;
        self
    }

    /// Sets the maximum number of deletions permitted during execution.
    #[must_use]
    #[doc(alias = "--max-delete")]
    pub const fn max_delete(mut self, limit: Option<u64>) -> Self {
        self.max_delete = limit;
        self
    }
}
