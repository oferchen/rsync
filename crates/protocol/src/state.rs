//! Type-safe state machine for rsync protocol phases.
//!
//! This module implements a typestate pattern for tracking the rsync protocol lifecycle,
//! making invalid state transitions a compile-time error where possible.
//!
//! # Protocol Phases
//!
//! The rsync protocol progresses through these phases:
//!
//! 1. **Negotiation** - Protocol version and capability negotiation
//! 2. **FileList** - File list exchange between sender and receiver
//! 3. **Transfer** - Delta transfer phase
//! 4. **Finalize** - Final statistics exchange and cleanup
//!
//! # Examples
//!
//! ```
//! use protocol::state::{ProtocolState, Negotiation};
//!
//! // Start in negotiation phase
//! let mut state = ProtocolState::<Negotiation>::new();
//! state.set_protocol_version(31);
//! state.set_checksum_seed(12345);
//!
//! // Transition to file list phase (compile-time type safety)
//! let mut state = state.begin_file_list().unwrap();
//! state.set_file_count(100);
//!
//! // Transition to transfer phase
//! let mut state = state.begin_transfer().unwrap();
//! state.record_transfer();
//! assert_eq!(state.files_transferred(), 1);
//!
//! // Transition to finalize phase
//! let state = state.begin_finalize();
//! let summary = state.summary();
//! assert_eq!(summary.protocol_version, 31);
//! assert_eq!(summary.total_files, 100);
//! assert_eq!(summary.files_transferred, 1);
//! ```

use std::fmt;

/// Protocol phase marker traits for type-safe transitions.
pub trait ProtocolPhase: fmt::Debug + Send + Sync {
    /// Human-readable name of this phase.
    fn name(&self) -> &'static str;
}

/// Negotiation phase - protocol version and capability exchange.
#[derive(Debug, Clone, Default)]
pub struct Negotiation {
    /// The negotiated protocol version, if set.
    pub protocol_version: Option<u32>,
    /// The checksum seed, if set.
    pub checksum_seed: Option<u32>,
}

/// FileList phase - file list exchange between sender and receiver.
#[derive(Debug, Clone)]
pub struct FileList {
    /// The negotiated protocol version.
    pub protocol_version: u32,
    /// The checksum seed for the session.
    pub checksum_seed: u32,
    /// The number of files in the list, if known.
    pub file_count: Option<usize>,
}

/// Transfer phase - delta transfer of file contents.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// The negotiated protocol version.
    pub protocol_version: u32,
    /// The checksum seed for the session.
    pub checksum_seed: u32,
    /// The total number of files to transfer.
    pub file_count: usize,
    /// The number of files transferred so far.
    pub files_transferred: usize,
}

/// Finalize phase - statistics exchange and cleanup.
#[derive(Debug, Clone)]
pub struct Finalize {
    /// The negotiated protocol version.
    pub protocol_version: u32,
    /// The total number of files.
    pub total_files: usize,
    /// The number of files transferred.
    pub files_transferred: usize,
}

impl ProtocolPhase for Negotiation {
    fn name(&self) -> &'static str {
        "negotiation"
    }
}

impl ProtocolPhase for FileList {
    fn name(&self) -> &'static str {
        "file_list"
    }
}

impl ProtocolPhase for Transfer {
    fn name(&self) -> &'static str {
        "transfer"
    }
}

impl ProtocolPhase for Finalize {
    fn name(&self) -> &'static str {
        "finalize"
    }
}

/// The protocol state machine parameterized by phase.
///
/// This uses the typestate pattern to ensure only valid transitions can occur.
/// Invalid transitions are caught at compile time.
#[derive(Debug)]
pub struct ProtocolState<P: ProtocolPhase> {
    phase: P,
}

impl ProtocolState<Negotiation> {
    /// Create a new protocol state machine starting in negotiation.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let state = ProtocolState::<Negotiation>::new();
    /// ```
    pub fn new() -> Self {
        Self {
            phase: Negotiation::default(),
        }
    }

    /// Set the negotiated protocol version.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// ```
    pub fn set_protocol_version(&mut self, version: u32) {
        self.phase.protocol_version = Some(version);
    }

    /// Set the checksum seed.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_checksum_seed(12345);
    /// ```
    pub fn set_checksum_seed(&mut self, seed: u32) {
        self.phase.checksum_seed = Some(seed);
    }

    /// Transition to file list phase.
    ///
    /// Requires protocol_version and checksum_seed to be set.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError::MissingProtocolVersion`] if the protocol version
    /// has not been set.
    ///
    /// Returns [`TransitionError::MissingChecksumSeed`] if the checksum seed
    /// has not been set.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// state.set_checksum_seed(12345);
    ///
    /// let file_list_state = state.begin_file_list().unwrap();
    /// ```
    pub fn begin_file_list(self) -> Result<ProtocolState<FileList>, TransitionError> {
        let protocol_version = self
            .phase
            .protocol_version
            .ok_or(TransitionError::MissingProtocolVersion)?;
        let checksum_seed = self
            .phase
            .checksum_seed
            .ok_or(TransitionError::MissingChecksumSeed)?;

        Ok(ProtocolState {
            phase: FileList {
                protocol_version,
                checksum_seed,
                file_count: None,
            },
        })
    }
}

impl Default for ProtocolState<Negotiation> {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolState<FileList> {
    /// Set the number of files in the list.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// state.set_checksum_seed(12345);
    /// let mut state = state.begin_file_list().unwrap();
    ///
    /// state.set_file_count(100);
    /// ```
    pub fn set_file_count(&mut self, count: usize) {
        self.phase.file_count = Some(count);
    }

    /// Transition to transfer phase.
    ///
    /// Requires file_count to be set.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError::MissingFileCount`] if the file count
    /// has not been set.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// state.set_checksum_seed(12345);
    /// let mut state = state.begin_file_list().unwrap();
    /// state.set_file_count(100);
    ///
    /// let transfer_state = state.begin_transfer().unwrap();
    /// ```
    pub fn begin_transfer(self) -> Result<ProtocolState<Transfer>, TransitionError> {
        let file_count = self
            .phase
            .file_count
            .ok_or(TransitionError::MissingFileCount)?;

        Ok(ProtocolState {
            phase: Transfer {
                protocol_version: self.phase.protocol_version,
                checksum_seed: self.phase.checksum_seed,
                file_count,
                files_transferred: 0,
            },
        })
    }
}

impl ProtocolState<Transfer> {
    /// Record a file transfer.
    ///
    /// Increments the files_transferred counter.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// state.set_checksum_seed(12345);
    /// let mut state = state.begin_file_list().unwrap();
    /// state.set_file_count(100);
    /// let mut state = state.begin_transfer().unwrap();
    ///
    /// state.record_transfer();
    /// assert_eq!(state.files_transferred(), 1);
    /// ```
    pub fn record_transfer(&mut self) {
        self.phase.files_transferred += 1;
    }

    /// Get the number of files transferred so far.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// state.set_checksum_seed(12345);
    /// let mut state = state.begin_file_list().unwrap();
    /// state.set_file_count(100);
    /// let mut state = state.begin_transfer().unwrap();
    ///
    /// assert_eq!(state.files_transferred(), 0);
    /// state.record_transfer();
    /// assert_eq!(state.files_transferred(), 1);
    /// ```
    pub fn files_transferred(&self) -> usize {
        self.phase.files_transferred
    }

    /// Transition to finalize phase.
    ///
    /// This transition is always valid and consumes the transfer state.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// state.set_checksum_seed(12345);
    /// let mut state = state.begin_file_list().unwrap();
    /// state.set_file_count(100);
    /// let state = state.begin_transfer().unwrap();
    ///
    /// let finalize_state = state.begin_finalize();
    /// ```
    pub fn begin_finalize(self) -> ProtocolState<Finalize> {
        ProtocolState {
            phase: Finalize {
                protocol_version: self.phase.protocol_version,
                total_files: self.phase.file_count,
                files_transferred: self.phase.files_transferred,
            },
        }
    }
}

impl ProtocolState<Finalize> {
    /// Get final transfer statistics.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::state::{ProtocolState, Negotiation};
    ///
    /// let mut state = ProtocolState::<Negotiation>::new();
    /// state.set_protocol_version(31);
    /// state.set_checksum_seed(12345);
    /// let mut state = state.begin_file_list().unwrap();
    /// state.set_file_count(100);
    /// let mut state = state.begin_transfer().unwrap();
    /// state.record_transfer();
    /// let state = state.begin_finalize();
    ///
    /// let summary = state.summary();
    /// assert_eq!(summary.protocol_version, 31);
    /// assert_eq!(summary.total_files, 100);
    /// assert_eq!(summary.files_transferred, 1);
    /// ```
    pub fn summary(&self) -> FinalizeSummary {
        FinalizeSummary {
            protocol_version: self.phase.protocol_version,
            total_files: self.phase.total_files,
            files_transferred: self.phase.files_transferred,
        }
    }
}

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
    checksum_seed: Option<u32>,
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

#[cfg(test)]
mod tests {
    use super::*;

    // Typestate tests

    #[test]
    fn test_new_starts_in_negotiation() {
        let state = ProtocolState::<Negotiation>::new();
        assert_eq!(state.phase.name(), "negotiation");
        assert_eq!(state.phase.protocol_version, None);
        assert_eq!(state.phase.checksum_seed, None);
    }

    #[test]
    fn test_negotiation_to_file_list() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);

        let file_list_state = state.begin_file_list().unwrap();
        assert_eq!(file_list_state.phase.name(), "file_list");
        assert_eq!(file_list_state.phase.protocol_version, 31);
        assert_eq!(file_list_state.phase.checksum_seed, 12345);
        assert_eq!(file_list_state.phase.file_count, None);
    }

    #[test]
    fn test_negotiation_to_file_list_missing_version() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_checksum_seed(12345);

        let result = state.begin_file_list();
        assert!(matches!(
            result,
            Err(TransitionError::MissingProtocolVersion)
        ));
    }

    #[test]
    fn test_negotiation_to_file_list_missing_seed() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_protocol_version(31);

        let result = state.begin_file_list();
        assert!(matches!(result, Err(TransitionError::MissingChecksumSeed)));
    }

    #[test]
    fn test_file_list_to_transfer() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);
        let mut file_list_state = state.begin_file_list().unwrap();
        file_list_state.set_file_count(100);

        let transfer_state = file_list_state.begin_transfer().unwrap();
        assert_eq!(transfer_state.phase.name(), "transfer");
        assert_eq!(transfer_state.phase.protocol_version, 31);
        assert_eq!(transfer_state.phase.checksum_seed, 12345);
        assert_eq!(transfer_state.phase.file_count, 100);
        assert_eq!(transfer_state.phase.files_transferred, 0);
    }

    #[test]
    fn test_file_list_to_transfer_missing_count() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);
        let file_list_state = state.begin_file_list().unwrap();

        let result = file_list_state.begin_transfer();
        assert!(matches!(result, Err(TransitionError::MissingFileCount)));
    }

    #[test]
    fn test_transfer_to_finalize() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);
        let mut file_list_state = state.begin_file_list().unwrap();
        file_list_state.set_file_count(100);
        let transfer_state = file_list_state.begin_transfer().unwrap();

        let finalize_state = transfer_state.begin_finalize();
        assert_eq!(finalize_state.phase.name(), "finalize");
        assert_eq!(finalize_state.phase.protocol_version, 31);
        assert_eq!(finalize_state.phase.total_files, 100);
        assert_eq!(finalize_state.phase.files_transferred, 0);
    }

    #[test]
    fn test_record_transfer_increments() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);
        let mut file_list_state = state.begin_file_list().unwrap();
        file_list_state.set_file_count(100);
        let mut transfer_state = file_list_state.begin_transfer().unwrap();

        assert_eq!(transfer_state.files_transferred(), 0);
        transfer_state.record_transfer();
        assert_eq!(transfer_state.files_transferred(), 1);
        transfer_state.record_transfer();
        assert_eq!(transfer_state.files_transferred(), 2);
    }

    #[test]
    fn test_finalize_summary() {
        let mut state = ProtocolState::<Negotiation>::new();
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);
        let mut file_list_state = state.begin_file_list().unwrap();
        file_list_state.set_file_count(100);
        let mut transfer_state = file_list_state.begin_transfer().unwrap();
        transfer_state.record_transfer();
        transfer_state.record_transfer();
        let finalize_state = transfer_state.begin_finalize();

        let summary = finalize_state.summary();
        assert_eq!(summary.protocol_version, 31);
        assert_eq!(summary.total_files, 100);
        assert_eq!(summary.files_transferred, 2);
    }

    #[test]
    fn test_full_lifecycle() {
        // Start in negotiation
        let mut state = ProtocolState::<Negotiation>::new();
        assert_eq!(state.phase.name(), "negotiation");

        // Set negotiation parameters
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);

        // Transition to file list
        let mut state = state.begin_file_list().unwrap();
        assert_eq!(state.phase.name(), "file_list");
        assert_eq!(state.phase.protocol_version, 31);
        assert_eq!(state.phase.checksum_seed, 12345);

        // Set file count
        state.set_file_count(50);

        // Transition to transfer
        let mut state = state.begin_transfer().unwrap();
        assert_eq!(state.phase.name(), "transfer");
        assert_eq!(state.phase.file_count, 50);

        // Record some transfers
        for _ in 0..10 {
            state.record_transfer();
        }
        assert_eq!(state.files_transferred(), 10);

        // Transition to finalize
        let state = state.begin_finalize();
        assert_eq!(state.phase.name(), "finalize");

        // Check summary
        let summary = state.summary();
        assert_eq!(summary.protocol_version, 31);
        assert_eq!(summary.total_files, 50);
        assert_eq!(summary.files_transferred, 10);
    }

    // Dynamic state tests

    #[test]
    fn test_dynamic_new() {
        let state = DynamicProtocolState::new();
        assert_eq!(state.phase(), Phase::Negotiation);
        assert_eq!(state.files_transferred(), 0);
    }

    #[test]
    fn test_dynamic_advance() {
        let mut state = DynamicProtocolState::new();

        // Negotiation -> FileList
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);
        let phase = state.advance().unwrap();
        assert_eq!(phase, Phase::FileList);
        assert_eq!(state.phase(), Phase::FileList);

        // FileList -> Transfer
        state.set_file_count(100);
        let phase = state.advance().unwrap();
        assert_eq!(phase, Phase::Transfer);
        assert_eq!(state.phase(), Phase::Transfer);

        // Transfer -> Finalize
        state.record_transfer();
        state.record_transfer();
        let phase = state.advance().unwrap();
        assert_eq!(phase, Phase::Finalize);
        assert_eq!(state.phase(), Phase::Finalize);

        // Finalize -> Finalize (stays in final state)
        let phase = state.advance().unwrap();
        assert_eq!(phase, Phase::Finalize);
        assert_eq!(state.phase(), Phase::Finalize);
    }

    #[test]
    fn test_dynamic_advance_without_prerequisites() {
        let mut state = DynamicProtocolState::new();

        // Missing protocol version
        state.set_checksum_seed(12345);
        let result = state.advance();
        assert!(matches!(
            result,
            Err(TransitionError::MissingProtocolVersion)
        ));

        // Set protocol version, still missing checksum seed
        state.set_protocol_version(31);
        state.checksum_seed = None; // Clear it
        let result = state.advance();
        assert!(matches!(result, Err(TransitionError::MissingChecksumSeed)));

        // Complete negotiation and advance
        state.set_checksum_seed(12345);
        state.advance().unwrap();
        assert_eq!(state.phase(), Phase::FileList);

        // Missing file count
        let result = state.advance();
        assert!(matches!(result, Err(TransitionError::MissingFileCount)));
    }

    #[test]
    fn test_phase_display() {
        assert_eq!(Phase::Negotiation.to_string(), "negotiation");
        assert_eq!(Phase::FileList.to_string(), "file_list");
        assert_eq!(Phase::Transfer.to_string(), "transfer");
        assert_eq!(Phase::Finalize.to_string(), "finalize");
    }

    #[test]
    fn test_dynamic_summary() {
        let mut state = DynamicProtocolState::new();

        // Summary not available before finalize
        assert_eq!(state.summary(), None);

        // Advance to finalize
        state.set_protocol_version(31);
        state.set_checksum_seed(12345);
        state.advance().unwrap();
        state.set_file_count(100);
        state.advance().unwrap();
        state.record_transfer();
        state.record_transfer();
        state.record_transfer();
        state.advance().unwrap();

        // Summary available in finalize
        let summary = state.summary().unwrap();
        assert_eq!(summary.protocol_version, 31);
        assert_eq!(summary.total_files, 100);
        assert_eq!(summary.files_transferred, 3);
    }
}
