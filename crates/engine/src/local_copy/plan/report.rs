use std::path::{Path, PathBuf};

use super::{LocalCopyRecord, LocalCopySummary};

/// Report returned after executing a [`crate::local_copy::LocalCopyPlan`] with event collection enabled.
#[derive(Clone, Debug, Default)]
pub struct LocalCopyReport {
    summary: LocalCopySummary,
    records: Vec<LocalCopyRecord>,
    destination_root: PathBuf,
}

impl LocalCopyReport {
    pub(in crate::local_copy) fn new(
        summary: LocalCopySummary,
        records: Vec<LocalCopyRecord>,
        destination_root: PathBuf,
    ) -> Self {
        Self {
            summary,
            records,
            destination_root,
        }
    }

    /// Returns the high-level summary collected during execution.
    #[must_use]
    pub const fn summary(&self) -> &LocalCopySummary {
        &self.summary
    }

    /// Consumes the report and returns the aggregated summary.
    #[must_use]
    pub fn into_summary(self) -> LocalCopySummary {
        self.summary
    }

    /// Returns the list of records captured during execution.
    #[must_use]
    pub fn records(&self) -> &[LocalCopyRecord] {
        &self.records
    }

    /// Consumes the report and returns the recorded events.
    #[must_use]
    pub fn into_records(self) -> Vec<LocalCopyRecord> {
        self.records
    }

    /// Returns the destination root path used during execution.
    #[must_use]
    pub fn destination_root(&self) -> &Path {
        &self.destination_root
    }
}
