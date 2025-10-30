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
