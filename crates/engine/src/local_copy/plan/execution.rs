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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_dry_run_returns_true_for_dry_run() {
        assert!(LocalCopyExecution::DryRun.is_dry_run());
    }

    #[test]
    fn is_dry_run_returns_false_for_apply() {
        assert!(!LocalCopyExecution::Apply.is_dry_run());
    }

    #[test]
    fn execution_clone_produces_equal_value() {
        let exec = LocalCopyExecution::Apply;
        let cloned = exec;
        assert_eq!(exec, cloned);
    }

    #[test]
    fn execution_copy_produces_equal_value() {
        let exec = LocalCopyExecution::DryRun;
        let copied: LocalCopyExecution = exec;
        assert_eq!(exec, copied);
    }

    #[test]
    fn execution_debug_format_contains_variant_name() {
        let apply = LocalCopyExecution::Apply;
        assert!(format!("{apply:?}").contains("Apply"));

        let dry_run = LocalCopyExecution::DryRun;
        assert!(format!("{dry_run:?}").contains("DryRun"));
    }

    #[test]
    fn execution_equality_same_variant() {
        assert_eq!(LocalCopyExecution::Apply, LocalCopyExecution::Apply);
        assert_eq!(LocalCopyExecution::DryRun, LocalCopyExecution::DryRun);
    }

    #[test]
    fn execution_inequality_different_variants() {
        assert_ne!(LocalCopyExecution::Apply, LocalCopyExecution::DryRun);
    }
}
