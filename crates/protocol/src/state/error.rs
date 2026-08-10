//! Error types and summary structures for protocol state transitions.

/// Error type for invalid state transitions.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TransitionError {
    /// Protocol version was not negotiated before transitioning.
    #[error("protocol version not negotiated")]
    MissingProtocolVersion,
    /// Checksum seed was not set before transitioning.
    #[error("checksum seed not set")]
    MissingChecksumSeed,
    /// File count was not set before transitioning.
    #[error("file count not set")]
    MissingFileCount,
}

/// Summary of a completed protocol session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeSummary {
    /// The protocol version used for the session.
    pub protocol_version: u32,
    /// The total number of files processed.
    pub total_files: usize,
    /// The number of files transferred.
    pub files_transferred: usize,
}
