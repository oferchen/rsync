use super::summary::ClientSummary;

/// Outcome returned when executing a client transfer.
#[derive(Debug)]
pub enum ClientOutcome {
    /// The transfer was handled by the local copy engine.
    Local(Box<ClientSummary>),
    /// The transfer was delegated to an upstream `rsync` binary.
    Fallback(FallbackSummary),
}

impl ClientOutcome {
    /// Returns the contained [`ClientSummary`] when the outcome represents a local execution.
    #[must_use]
    pub fn into_local(self) -> Option<ClientSummary> {
        match self {
            Self::Local(summary) => Some(*summary),
            Self::Fallback(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsync_engine::LocalCopySummary;

    fn empty_summary() -> ClientSummary {
        ClientSummary::from_summary(LocalCopySummary::default())
    }

    #[test]
    fn into_local_returns_summary_for_local_execution() {
        let summary = empty_summary();
        let outcome = ClientOutcome::Local(Box::new(summary));

        let extracted = outcome
            .into_local()
            .expect("local outcome should yield a summary");

        assert_eq!(extracted.files_copied(), 0);
        assert!(extracted.events().is_empty());
    }

    #[test]
    fn into_local_returns_none_for_fallback_execution() {
        let outcome = ClientOutcome::Fallback(FallbackSummary::new(23));

        assert!(outcome.into_local().is_none());
    }

    #[test]
    fn fallback_summary_reports_exit_code() {
        let summary = FallbackSummary::new(42);

        assert_eq!(summary.exit_code(), 42);
    }
}

/// Summary describing the result of a fallback invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FallbackSummary {
    exit_code: i32,
}

impl FallbackSummary {
    pub(crate) const fn new(exit_code: i32) -> Self {
        Self { exit_code }
    }

    /// Returns the exit code reported by the fallback process.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        self.exit_code
    }
}
