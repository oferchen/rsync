//! Compile-time type-safe protocol state machine using the typestate pattern.
//!
//! [`ProtocolState`] is parameterized by a phase marker type, ensuring only valid
//! transitions can occur. Invalid transitions are caught at compile time.

use super::error::{FinalizeSummary, TransitionError};
use super::phases::{FileList, Finalize, Negotiation, ProtocolPhase, Transfer};

/// The protocol state machine parameterized by phase.
///
/// This uses the typestate pattern to ensure only valid transitions can occur.
/// Invalid transitions are caught at compile time.
#[derive(Debug)]
pub struct ProtocolState<P: ProtocolPhase> {
    pub(crate) phase: P,
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
