//! Runtime-checked dynamic protocol state tracker.
//!
//! [`DynamicProtocolState`] provides phase tracking when the typestate pattern
//! is not practical - e.g., when phase transitions depend on runtime conditions.

use std::fmt;

use super::error::{FinalizeSummary, TransitionError};

/// Dynamic phase enum for runtime state tracking (when typestate isn't practical).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Negotiation phase.
    Negotiation,
    /// File list exchange phase.
    FileList,
    /// Transfer phase.
    Transfer,
    /// Finalize phase.
    Finalize,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Negotiation => write!(f, "negotiation"),
            Phase::FileList => write!(f, "file_list"),
            Phase::Transfer => write!(f, "transfer"),
            Phase::Finalize => write!(f, "finalize"),
        }
    }
}

/// Dynamic protocol tracker that doesn't use typestate.
///
/// Useful when the phase needs to be tracked at runtime rather than
/// enforced at compile time.
///
/// # Examples
///
/// ```
/// use protocol::state::{DynamicProtocolState, Phase};
///
/// let mut state = DynamicProtocolState::new();
/// assert_eq!(state.phase(), Phase::Negotiation);
///
/// state.set_protocol_version(31);
/// state.set_checksum_seed(12345);
/// state.advance().unwrap();
/// assert_eq!(state.phase(), Phase::FileList);
///
/// state.set_file_count(100);
/// state.advance().unwrap();
/// assert_eq!(state.phase(), Phase::Transfer);
///
/// state.record_transfer();
/// state.advance().unwrap();
/// assert_eq!(state.phase(), Phase::Finalize);
/// ```
#[derive(Debug)]
pub struct DynamicProtocolState {
    phase: Phase,
    protocol_version: Option<u32>,
    pub(crate) checksum_seed: Option<u32>,
    file_count: Option<usize>,
    files_transferred: usize,
}

impl DynamicProtocolState {
    /// Create a new dynamic protocol state starting in negotiation.
    pub fn new() -> Self {
        Self {
            phase: Phase::Negotiation,
            protocol_version: None,
            checksum_seed: None,
            file_count: None,
            files_transferred: 0,
        }
    }

    /// Get the current phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Set the protocol version.
    pub fn set_protocol_version(&mut self, version: u32) {
        self.protocol_version = Some(version);
    }

    /// Set the checksum seed.
    pub fn set_checksum_seed(&mut self, seed: u32) {
        self.checksum_seed = Some(seed);
    }

    /// Set the file count.
    pub fn set_file_count(&mut self, count: usize) {
        self.file_count = Some(count);
    }

    /// Record a file transfer.
    pub fn record_transfer(&mut self) {
        self.files_transferred += 1;
    }

    /// Get the number of files transferred.
    pub fn files_transferred(&self) -> usize {
        self.files_transferred
    }

    /// Advance to the next phase.
    ///
    /// # Errors
    ///
    /// Returns an error if the required fields for the transition are not set.
    pub fn advance(&mut self) -> Result<Phase, TransitionError> {
        match self.phase {
            Phase::Negotiation => {
                self.protocol_version
                    .ok_or(TransitionError::MissingProtocolVersion)?;
                self.checksum_seed
                    .ok_or(TransitionError::MissingChecksumSeed)?;
                self.phase = Phase::FileList;
                Ok(Phase::FileList)
            }
            Phase::FileList => {
                self.file_count.ok_or(TransitionError::MissingFileCount)?;
                self.phase = Phase::Transfer;
                Ok(Phase::Transfer)
            }
            Phase::Transfer => {
                self.phase = Phase::Finalize;
                Ok(Phase::Finalize)
            }
            Phase::Finalize => {
                // Already in final state, return current phase
                Ok(Phase::Finalize)
            }
        }
    }

    /// Get a summary of the session (only valid in finalize phase).
    ///
    /// Returns `None` if not in the finalize phase.
    pub fn summary(&self) -> Option<FinalizeSummary> {
        if self.phase == Phase::Finalize {
            Some(FinalizeSummary {
                protocol_version: self.protocol_version?,
                total_files: self.file_count?,
                files_transferred: self.files_transferred,
            })
        } else {
            None
        }
    }
}

impl Default for DynamicProtocolState {
    fn default() -> Self {
        Self::new()
    }
}
