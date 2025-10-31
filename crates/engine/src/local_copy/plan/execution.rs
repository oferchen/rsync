/// Describes how a [`super::LocalCopyPlan`] should be executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyExecution {
    /// Perform the copy and mutate the destination filesystem.
    Apply,
    /// Validate the copy without mutating the destination tree.
    DryRun,
}

impl LocalCopyExecution {
    pub(in crate::local_copy) const fn is_dry_run(self) -> bool {
        matches!(self, Self::DryRun)
    }
}
