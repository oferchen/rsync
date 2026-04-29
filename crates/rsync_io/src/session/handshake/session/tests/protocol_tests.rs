//! Tests for protocol version query accessors.

use crate::handshake_util::RemoteProtocolAdvertisement;
use crate::session::handshake::SessionHandshake;
use protocol::NegotiationPrologue;

use super::helpers::{create_binary_handshake, create_legacy_handshake};

#[test]
fn decision_returns_binary_for_binary_variant() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert_eq!(session.decision(), NegotiationPrologue::Binary);
}

#[test]
fn decision_returns_legacy_for_legacy_variant() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert_eq!(session.decision(), NegotiationPrologue::LegacyAscii);
}

#[test]
fn is_binary_true_for_binary_variant() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(session.is_binary());
    assert!(!session.is_legacy());
}

#[test]
fn is_legacy_true_for_legacy_variant() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(session.is_legacy());
    assert!(!session.is_binary());
}

#[test]
fn negotiated_protocol_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert_eq!(session.negotiated_protocol().as_u8(), 31);
}

#[test]
fn negotiated_protocol_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert_eq!(session.negotiated_protocol().as_u8(), 31);
}

#[test]
fn remote_protocol_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert_eq!(session.remote_protocol().as_u8(), 31);
}

#[test]
fn remote_protocol_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert_eq!(session.remote_protocol().as_u8(), 31);
}

#[test]
fn remote_advertised_protocol_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert_eq!(session.remote_advertised_protocol(), 31);
}

#[test]
fn remote_advertised_protocol_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert_eq!(session.remote_advertised_protocol(), 31);
}

#[test]
fn local_advertised_protocol_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert_eq!(session.local_advertised_protocol().as_u8(), 31);
}

#[test]
fn local_advertised_protocol_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert_eq!(session.local_advertised_protocol().as_u8(), 31);
}

#[test]
fn server_greeting_none_for_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(session.server_greeting().is_none());
}

#[test]
fn server_greeting_some_for_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(session.server_greeting().is_some());
}

#[test]
fn remote_advertisement_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let adv = session.remote_advertisement();
    assert!(matches!(adv, RemoteProtocolAdvertisement::Supported(_)));
}

#[test]
fn remote_advertisement_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let adv = session.remote_advertisement();
    assert!(matches!(adv, RemoteProtocolAdvertisement::Supported(_)));
}

#[test]
fn remote_protocol_was_clamped_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(!session.remote_protocol_was_clamped());
}

#[test]
fn remote_protocol_was_clamped_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(!session.remote_protocol_was_clamped());
}

#[test]
fn local_protocol_was_capped_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(!session.local_protocol_was_capped());
}

#[test]
fn local_protocol_was_capped_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(!session.local_protocol_was_capped());
}

#[test]
fn clone_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let cloned = session.clone();
    assert!(cloned.is_binary());
    assert_eq!(session.negotiated_protocol(), cloned.negotiated_protocol());
}

#[test]
fn clone_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let cloned = session.clone();
    assert!(cloned.is_legacy());
    assert_eq!(session.negotiated_protocol(), cloned.negotiated_protocol());
}

#[test]
fn debug_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let debug = format!("{session:?}");
    assert!(debug.contains("Binary"));
}

#[test]
fn debug_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let debug = format!("{session:?}");
    assert!(debug.contains("Legacy"));
}
