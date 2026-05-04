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
//! # Submodules
//!
//! - `phases` - Phase marker types for the typestate pattern
//! - `typestate` - Compile-time type-safe state machine
//! - `dynamic` - Runtime-checked dynamic state tracker
//! - `error` - Transition errors and summary types
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

mod dynamic;
mod error;
mod phases;
mod typestate;

pub use dynamic::{DynamicProtocolState, Phase};
pub use error::{FinalizeSummary, TransitionError};
pub use phases::{FileList, Finalize, Negotiation, ProtocolPhase, Transfer};
pub use typestate::ProtocolState;

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
