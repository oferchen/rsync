use std::ffi::OsString;
use std::path::Path;

use super::super::{
    CopyOutcome, DestinationSpec, LocalCopyArgumentError, LocalCopyError, LocalCopyOptions,
    SourceSpec, copy_sources, operand_is_remote,
};
use super::{LocalCopyExecution, LocalCopyRecordHandler, LocalCopyReport, LocalCopySummary};

/// Plan describing a local filesystem copy.
///
/// Instances are constructed from CLI-style operands using
/// [`LocalCopyPlan::from_operands`]. Execution copies regular files, directories,
/// and symbolic links while preserving permissions, timestamps, and
/// optional ownership metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCopyPlan {
    pub(super) sources: Vec<SourceSpec>,
    pub(super) destination: DestinationSpec,
}

impl LocalCopyPlan {
    /// Constructs a plan from CLI-style operands.
    ///
    /// The operands must contain at least one source and a destination. A
    /// trailing path separator on a source operand mirrors upstream rsync's
    /// behaviour of copying the directory *contents* rather than the directory
    /// itself. Remote operands such as `host::module`, `host:/path`, or
    /// `rsync://server/module` are rejected with
    /// [`LocalCopyArgumentError::RemoteOperandUnsupported`] so callers receive a
    /// deterministic diagnostic explaining that this build only supports local
    /// filesystem copies.
    ///
    /// # Errors
    ///
    /// Returns [`crate::local_copy::LocalCopyErrorKind::MissingSourceOperands`] when fewer than two
    /// operands are supplied. Empty operands and invalid destination states are
    /// reported via [`crate::local_copy::LocalCopyErrorKind::InvalidArgument`].
    ///
    /// # Examples
    ///
    /// ```
    /// use engine::local_copy::LocalCopyPlan;
    /// use std::ffi::OsString;
    ///
    /// let operands = vec![OsString::from("src"), OsString::from("dst")];
    /// let plan = LocalCopyPlan::from_operands(&operands).expect("plan succeeds");
    /// assert_eq!(plan.destination(), std::path::Path::new("dst"));
    /// ```
    pub fn from_operands(operands: &[OsString]) -> Result<Self, LocalCopyError> {
        if operands.len() < 2 {
            return Err(LocalCopyError::missing_operands());
        }

        let sources: Vec<SourceSpec> = operands[..operands.len() - 1]
            .iter()
            .map(SourceSpec::from_operand)
            .collect::<Result<_, _>>()?;

        if sources.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptySourceOperand,
            ));
        }

        let destination_operand = &operands[operands.len() - 1];
        if destination_operand.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptyDestinationOperand,
            ));
        }

        if operand_is_remote(destination_operand.as_os_str()) {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::RemoteOperandUnsupported,
            ));
        }

        let destination = DestinationSpec::from_operand(destination_operand);

        Ok(Self {
            sources,
            destination,
        })
    }

    /// Returns the planned source operands.
    #[must_use]
    pub(crate) fn sources(&self) -> &[SourceSpec] {
        &self.sources
    }

    /// Returns the planned destination path.
    #[must_use]
    pub fn destination(&self) -> &Path {
        self.destination.path()
    }

    pub(in crate::local_copy) fn destination_spec(&self) -> &DestinationSpec {
        &self.destination
    }

    /// Executes the planned copy.
    ///
    /// # Errors
    ///
    /// Reports [`LocalCopyError`] variants when operand validation fails or I/O
    /// operations encounter errors.
    pub fn execute(&self) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
    }

    /// Executes the planned copy using the requested execution mode.
    ///
    /// When [`LocalCopyExecution::DryRun`] is selected the filesystem is left
    /// untouched while operand validation and readability checks still occur.
    pub fn execute_with(
        &self,
        mode: LocalCopyExecution,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options(mode, LocalCopyOptions::default())
    }

    /// Executes the planned copy with additional behavioural options.
    pub fn execute_with_options(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options_and_handler(mode, options, None)
    }

    /// Executes the planned copy and returns a detailed report of performed actions.
    pub fn execute_with_report(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<LocalCopyReport, LocalCopyError> {
        self.execute_with_report_and_handler(mode, options, None)
    }

    /// Executes the planned copy while routing records to the supplied handler.
    pub fn execute_with_options_and_handler(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        handler: Option<&mut dyn LocalCopyRecordHandler>,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        copy_sources(self, mode, options, handler).map(CopyOutcome::into_summary)
    }

    /// Executes the planned copy, returning a detailed report and notifying the handler.
    pub fn execute_with_report_and_handler(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        handler: Option<&mut dyn LocalCopyRecordHandler>,
    ) -> Result<LocalCopyReport, LocalCopyError> {
        copy_sources(self, mode, options, handler).map(|outcome| {
            let (_summary, report) = outcome.into_summary_and_report();
            report
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ==================== from_operands tests ====================

    #[test]
    fn from_operands_single_source() {
        let operands = vec![OsString::from("src"), OsString::from("dst")];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        assert_eq!(plan.sources().len(), 1);
        assert_eq!(plan.destination(), Path::new("dst"));
    }

    #[test]
    fn from_operands_multiple_sources() {
        let operands = vec![
            OsString::from("src1"),
            OsString::from("src2"),
            OsString::from("dst"),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        assert_eq!(plan.sources().len(), 2);
    }

    #[test]
    fn from_operands_fewer_than_two_fails() {
        let operands = vec![OsString::from("only_one")];
        let result = LocalCopyPlan::from_operands(&operands);
        assert!(result.is_err());
    }

    #[test]
    fn from_operands_empty_fails() {
        let operands: Vec<OsString> = vec![];
        let result = LocalCopyPlan::from_operands(&operands);
        assert!(result.is_err());
    }

    #[test]
    fn from_operands_empty_destination_fails() {
        let operands = vec![OsString::from("src"), OsString::from("")];
        let result = LocalCopyPlan::from_operands(&operands);
        assert!(result.is_err());
    }

    #[test]
    fn from_operands_absolute_paths() {
        let operands = vec![OsString::from("/tmp/src"), OsString::from("/tmp/dst")];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        assert_eq!(plan.destination(), Path::new("/tmp/dst"));
    }

    #[test]
    fn from_operands_remote_destination_fails() {
        let operands = vec![OsString::from("src"), OsString::from("host::module")];
        let result = LocalCopyPlan::from_operands(&operands);
        assert!(result.is_err());
    }

    #[test]
    fn from_operands_rsync_url_destination_fails() {
        let operands = vec![
            OsString::from("src"),
            OsString::from("rsync://server/module"),
        ];
        let result = LocalCopyPlan::from_operands(&operands);
        assert!(result.is_err());
    }

    #[test]
    fn from_operands_ssh_style_destination_fails() {
        let operands = vec![OsString::from("src"), OsString::from("host:/path")];
        let result = LocalCopyPlan::from_operands(&operands);
        assert!(result.is_err());
    }

    // ==================== accessor tests ====================

    #[test]
    fn destination_returns_correct_path() {
        let operands = vec![OsString::from("source"), OsString::from("dest_dir")];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        assert_eq!(plan.destination(), Path::new("dest_dir"));
    }

    #[test]
    fn sources_returns_all_sources() {
        let operands = vec![
            OsString::from("a"),
            OsString::from("b"),
            OsString::from("c"),
            OsString::from("dest"),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        assert_eq!(plan.sources().len(), 3);
    }

    // ==================== Clone and Eq tests ====================

    #[test]
    fn plan_is_clone() {
        let operands = vec![OsString::from("src"), OsString::from("dst")];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        let cloned = plan.clone();
        assert_eq!(plan, cloned);
    }

    #[test]
    fn plan_is_eq() {
        let operands = vec![OsString::from("src"), OsString::from("dst")];
        let plan1 = LocalCopyPlan::from_operands(&operands).unwrap();
        let plan2 = LocalCopyPlan::from_operands(&operands).unwrap();
        assert_eq!(plan1, plan2);
    }

    #[test]
    fn plan_debug_format() {
        let operands = vec![OsString::from("src"), OsString::from("dst")];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        let debug = format!("{plan:?}");
        assert!(debug.contains("LocalCopyPlan"));
    }
}
