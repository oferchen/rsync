use super::summary::ClientSummary;

/// Outcome returned when executing a client transfer.
#[derive(Debug)]
pub enum ClientOutcome {
    /// The transfer was handled by the local copy engine.
    Local(Box<ClientSummary>),
}

impl ClientOutcome {
    /// Returns the contained [`ClientSummary`] when the outcome represents a local execution.
    #[must_use]
    pub fn into_local(self) -> Option<ClientSummary> {
        match self {
            Self::Local(summary) => Some(*summary),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::LocalCopySummary;

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
    fn into_local_extracts_summary() {
        let outcome = ClientOutcome::Local(Box::new(empty_summary()));

        let extracted = outcome
            .into_local()
            .expect("local outcome should yield a summary");

        assert_eq!(extracted.files_copied(), 0);
    }
}
