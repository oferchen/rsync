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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_report() {
        let report = LocalCopyReport::default();
        assert!(report.records().is_empty());
        assert_eq!(report.destination_root(), Path::new(""));
    }

    #[test]
    fn new_stores_values() {
        let summary = LocalCopySummary::default();
        let records = vec![];
        let dest = PathBuf::from("/dest");
        let report = LocalCopyReport::new(summary, records, dest.clone());
        assert_eq!(report.destination_root(), dest);
    }

    #[test]
    fn summary_returns_reference() {
        let report = LocalCopyReport::default();
        let _summary = report.summary();
    }

    #[test]
    fn into_summary_consumes_report() {
        let report = LocalCopyReport::default();
        let _summary = report.into_summary();
    }

    #[test]
    fn records_returns_slice() {
        let report = LocalCopyReport::default();
        assert!(report.records().is_empty());
    }

    #[test]
    fn into_records_consumes_report() {
        let report = LocalCopyReport::default();
        let records = report.into_records();
        assert!(records.is_empty());
    }

    #[test]
    fn destination_root_returns_path() {
        let dest = PathBuf::from("/my/destination");
        let report = LocalCopyReport::new(LocalCopySummary::default(), vec![], dest.clone());
        assert_eq!(report.destination_root(), dest);
    }

    #[test]
    fn clone_works() {
        let report = LocalCopyReport::default();
        let cloned = report;
        assert!(cloned.records().is_empty());
    }

    #[test]
    fn debug_format() {
        let report = LocalCopyReport::default();
        let debug = format!("{report:?}");
        assert!(debug.contains("LocalCopyReport"));
    }
}
