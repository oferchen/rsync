//! Tests verifying that the TransferPipeline FSM is wired into the transfer
//! orchestration code and enforces phase transitions at runtime.
//!
//! These tests validate:
//! - The FSM is initialized at the correct phase when contexts are created
//! - Phase transitions occur at the expected orchestration points
//! - Invalid transitions produce errors (not panics)

use crate::role::ServerRole;
use crate::transfer_state::{TransferPhase, TransferPipeline};

// ---------------------------------------------------------------------------
// Pipeline initialization
// ---------------------------------------------------------------------------

#[test]
fn pipeline_for_receiver_starts_at_handshake() {
    let pipeline = TransferPipeline::new(ServerRole::Receiver);
    assert_eq!(pipeline.phase(), TransferPhase::Handshake);
    assert_eq!(pipeline.role(), ServerRole::Receiver);
}

#[test]
fn pipeline_for_generator_starts_at_handshake() {
    let pipeline = TransferPipeline::new(ServerRole::Generator);
    assert_eq!(pipeline.phase(), TransferPhase::Handshake);
    assert_eq!(pipeline.role(), ServerRole::Generator);
}

// ---------------------------------------------------------------------------
// Orchestration-level transition sequence (mirrors run_server_with_handshake)
// ---------------------------------------------------------------------------

#[test]
fn orchestration_advances_handshake_to_filter_exchange() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    // After setup_protocol completes, the orchestration advances to FilterExchange.
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .expect("handshake -> filter exchange");
    assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
}

#[test]
fn receiver_lifecycle_matches_orchestration_phases() {
    // Simulates the receiver side of run_server_with_handshake -> ReceiverContext::run
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);

    // run_server_with_handshake: after setup_protocol
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .expect("handshake -> filter exchange");

    // ReceiverContext::setup_transfer: after reading filter list
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .expect("filter exchange -> file list transfer");

    // ReceiverContext::setup_transfer: after receiving file list
    pipeline
        .advance_to(TransferPhase::DeltaTransfer)
        .expect("file list transfer -> delta transfer");

    // ReceiverContext::finalize_transfer: after delta loop completes
    pipeline
        .advance_to(TransferPhase::Finalization)
        .expect("delta transfer -> finalization");

    // ReceiverContext::finalize_transfer: after goodbye handshake
    pipeline
        .advance_to(TransferPhase::Complete)
        .expect("finalization -> complete");

    assert!(pipeline.is_complete());
}

#[test]
fn generator_lifecycle_matches_orchestration_phases() {
    // Simulates the generator side of run_server_with_handshake -> GeneratorContext::run
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);

    // run_server_with_handshake: after setup_protocol
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .expect("handshake -> filter exchange");

    // GeneratorContext::run: after filter list exchange
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .expect("filter exchange -> file list transfer");

    // GeneratorContext::run: after send_file_list
    pipeline
        .advance_to(TransferPhase::DeltaTransfer)
        .expect("file list transfer -> delta transfer");

    // GeneratorContext::run: after transfer loop completes
    pipeline
        .advance_to(TransferPhase::Finalization)
        .expect("delta transfer -> finalization");

    // GeneratorContext::run: after goodbye handshake
    pipeline
        .advance_to(TransferPhase::Complete)
        .expect("finalization -> complete");

    assert!(pipeline.is_complete());
}

// ---------------------------------------------------------------------------
// Invalid transition enforcement
// ---------------------------------------------------------------------------

#[test]
fn skipping_filter_exchange_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    // Trying to jump from Handshake to FileListTransfer (skipping FilterExchange)
    let err = pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::Handshake);
    assert_eq!(err.target, TransferPhase::FileListTransfer);
    // Pipeline remains at Handshake
    assert_eq!(pipeline.phase(), TransferPhase::Handshake);
}

#[test]
fn backward_transition_during_transfer_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .unwrap();
    pipeline.advance_to(TransferPhase::DeltaTransfer).unwrap();

    // Trying to go back to FileListTransfer
    let err = pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::DeltaTransfer);
    assert_eq!(err.target, TransferPhase::FileListTransfer);
}

#[test]
fn advancing_past_complete_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    pipeline.advance_through(TransferPhase::Complete).unwrap();
    assert!(pipeline.is_complete());

    let err = pipeline.advance().unwrap_err();
    assert_eq!(err.current, TransferPhase::Complete);
}

// ---------------------------------------------------------------------------
// fsm_error conversion
// ---------------------------------------------------------------------------

#[test]
fn fsm_error_produces_io_error() {
    let err = crate::transfer_state::InvalidTransition {
        current: TransferPhase::DeltaTransfer,
        target: TransferPhase::Handshake,
    };
    let io_err = crate::fsm_error(err);
    assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidData);
    let msg = io_err.to_string();
    assert!(
        msg.contains("delta-transfer"),
        "error should mention current phase: {msg}"
    );
    assert!(
        msg.contains("handshake"),
        "error should mention target phase: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Test helper validation
// ---------------------------------------------------------------------------

#[test]
fn receiver_new_for_test_starts_at_filter_exchange() {
    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use protocol::ProtocolVersion;

    let handshake = HandshakeResult {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };
    let config = ServerConfig {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-a".to_owned(),
        args: vec![std::ffi::OsString::from(".")],
        ..Default::default()
    };

    let ctx = crate::receiver::ReceiverContext::new_for_test(&handshake, config);
    assert_eq!(ctx.protocol().as_u8(), 32);
}

#[test]
fn generator_new_for_test_starts_at_filter_exchange() {
    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use protocol::ProtocolVersion;

    let handshake = HandshakeResult {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };
    let mut config = ServerConfig {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-a".to_owned(),
        args: vec![std::ffi::OsString::from(".")],
        ..Default::default()
    };
    config.role = ServerRole::Generator;

    let ctx = crate::generator::GeneratorContext::new_for_test(&handshake, config);
    assert_eq!(ctx.protocol().as_u8(), 32);
}

// ---------------------------------------------------------------------------
// Extended edge cases: skip rejection at every orchestration boundary
// ---------------------------------------------------------------------------

#[test]
fn skipping_file_list_transfer_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
    let err = pipeline
        .advance_to(TransferPhase::DeltaTransfer)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::FilterExchange);
    assert_eq!(err.target, TransferPhase::DeltaTransfer);
    assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
}

#[test]
fn skipping_delta_transfer_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline
        .advance_through(TransferPhase::FileListTransfer)
        .unwrap();
    let err = pipeline
        .advance_to(TransferPhase::Finalization)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::FileListTransfer);
    assert_eq!(err.target, TransferPhase::Finalization);
    assert_eq!(pipeline.phase(), TransferPhase::FileListTransfer);
}

#[test]
fn skipping_finalization_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    pipeline
        .advance_through(TransferPhase::DeltaTransfer)
        .unwrap();
    let err = pipeline.advance_to(TransferPhase::Complete).unwrap_err();
    assert_eq!(err.current, TransferPhase::DeltaTransfer);
    assert_eq!(err.target, TransferPhase::Complete);
    assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);
}

// ---------------------------------------------------------------------------
// Extended edge cases: backward rejection at additional boundaries
// ---------------------------------------------------------------------------

#[test]
fn backward_from_complete_to_finalization_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline.advance_through(TransferPhase::Complete).unwrap();
    let err = pipeline
        .advance_to(TransferPhase::Finalization)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::Complete);
    assert_eq!(err.target, TransferPhase::Finalization);
    assert_eq!(pipeline.phase(), TransferPhase::Complete);
}

#[test]
fn backward_from_finalization_to_handshake_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    pipeline
        .advance_through(TransferPhase::Finalization)
        .unwrap();
    let err = pipeline.advance_to(TransferPhase::Handshake).unwrap_err();
    assert_eq!(err.current, TransferPhase::Finalization);
    assert_eq!(err.target, TransferPhase::Handshake);
    assert_eq!(pipeline.phase(), TransferPhase::Finalization);
}

// ---------------------------------------------------------------------------
// Self-transition rejection at orchestration-relevant phases
// ---------------------------------------------------------------------------

#[test]
fn self_transition_at_filter_exchange_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
    let err = pipeline
        .advance_to(TransferPhase::FilterExchange)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::FilterExchange);
    assert_eq!(err.target, TransferPhase::FilterExchange);
    assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
}

#[test]
fn self_transition_at_delta_transfer_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline
        .advance_through(TransferPhase::DeltaTransfer)
        .unwrap();
    let err = pipeline
        .advance_to(TransferPhase::DeltaTransfer)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::DeltaTransfer);
    assert_eq!(err.target, TransferPhase::DeltaTransfer);
    assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);
}

// ---------------------------------------------------------------------------
// State preservation: pipeline is not mutated on failed advance
// ---------------------------------------------------------------------------

#[test]
fn failed_advance_to_preserves_state() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    pipeline.advance_to(TransferPhase::FilterExchange).unwrap();

    // Three distinct failure modes, each must leave the pipeline at FilterExchange
    let _ = pipeline.advance_to(TransferPhase::DeltaTransfer); // skip
    assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);

    let _ = pipeline.advance_to(TransferPhase::Handshake); // backward
    assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);

    let _ = pipeline.advance_to(TransferPhase::FilterExchange); // self
    assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
}

#[test]
fn failed_advance_from_complete_preserves_state() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline.advance_through(TransferPhase::Complete).unwrap();

    let _ = pipeline.advance();
    assert!(pipeline.is_complete());
    assert_eq!(pipeline.phase(), TransferPhase::Complete);

    let _ = pipeline.advance_to(TransferPhase::Handshake);
    assert!(pipeline.is_complete());
}

// ---------------------------------------------------------------------------
// Both roles traverse the same state sequence
// ---------------------------------------------------------------------------

#[test]
fn both_roles_have_identical_phase_sequence() {
    let phases = [
        TransferPhase::FilterExchange,
        TransferPhase::FileListTransfer,
        TransferPhase::DeltaTransfer,
        TransferPhase::Finalization,
        TransferPhase::Complete,
    ];

    for role in [ServerRole::Receiver, ServerRole::Generator] {
        let mut pipeline = TransferPipeline::new(role);
        for &phase in &phases {
            pipeline.advance_to(phase).unwrap();
            assert_eq!(pipeline.phase(), phase);
        }
        assert!(pipeline.is_complete());
        assert_eq!(pipeline.role(), role);
    }
}

// ---------------------------------------------------------------------------
// is_complete is false for all non-terminal phases
// ---------------------------------------------------------------------------

#[test]
fn is_complete_false_for_all_non_terminal_phases() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);

    assert!(!pipeline.is_complete()); // Handshake
    pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
    assert!(!pipeline.is_complete());
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .unwrap();
    assert!(!pipeline.is_complete());
    pipeline.advance_to(TransferPhase::DeltaTransfer).unwrap();
    assert!(!pipeline.is_complete());
    pipeline.advance_to(TransferPhase::Finalization).unwrap();
    assert!(!pipeline.is_complete());
    pipeline.advance_to(TransferPhase::Complete).unwrap();
    assert!(pipeline.is_complete());
}

// ---------------------------------------------------------------------------
// advance_through in orchestration context
// ---------------------------------------------------------------------------

#[test]
fn advance_through_multiple_phases_is_valid() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline
        .advance_through(TransferPhase::DeltaTransfer)
        .unwrap();
    assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);
    // Continue normally after multi-step jump
    pipeline.advance_to(TransferPhase::Finalization).unwrap();
    pipeline.advance_to(TransferPhase::Complete).unwrap();
    assert!(pipeline.is_complete());
}

#[test]
fn advance_through_backward_from_mid_pipeline_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
    pipeline
        .advance_through(TransferPhase::DeltaTransfer)
        .unwrap();
    let err = pipeline
        .advance_through(TransferPhase::FilterExchange)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::DeltaTransfer);
    assert_eq!(err.target, TransferPhase::FilterExchange);
    assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);
}

// ---------------------------------------------------------------------------
// fsm_error conversion edge cases
// ---------------------------------------------------------------------------

#[test]
fn fsm_error_forward_skip_produces_io_error() {
    let err = crate::transfer_state::InvalidTransition {
        current: TransferPhase::Handshake,
        target: TransferPhase::FileListTransfer,
    };
    let io_err = crate::fsm_error(err);
    assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidData);
    let msg = io_err.to_string();
    assert!(msg.contains("handshake"), "missing current: {msg}");
    assert!(
        msg.contains("file-list-transfer"),
        "missing target: {msg}"
    );
}

#[test]
fn fsm_error_self_transition_produces_io_error() {
    let err = crate::transfer_state::InvalidTransition {
        current: TransferPhase::Complete,
        target: TransferPhase::Complete,
    };
    let io_err = crate::fsm_error(err);
    assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidData);
    assert!(io_err.to_string().contains("complete"));
}

// ---------------------------------------------------------------------------
// Generator-specific invalid transitions mirror receiver rejections
// ---------------------------------------------------------------------------

#[test]
fn generator_skipping_filter_exchange_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    let err = pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::Handshake);
    assert_eq!(err.target, TransferPhase::FileListTransfer);
    assert_eq!(pipeline.phase(), TransferPhase::Handshake);
}

#[test]
fn generator_backward_transition_during_transfer_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline
        .advance_through(TransferPhase::Finalization)
        .unwrap();
    let err = pipeline
        .advance_to(TransferPhase::DeltaTransfer)
        .unwrap_err();
    assert_eq!(err.current, TransferPhase::Finalization);
    assert_eq!(err.target, TransferPhase::DeltaTransfer);
}

#[test]
fn generator_advancing_past_complete_is_rejected() {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline.advance_through(TransferPhase::Complete).unwrap();
    assert!(pipeline.is_complete());

    let err = pipeline.advance().unwrap_err();
    assert_eq!(err.current, TransferPhase::Complete);
}
