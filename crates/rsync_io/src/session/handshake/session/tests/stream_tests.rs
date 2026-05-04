//! Tests for stream access, variant downcasts, and transport mapping.

use crate::binary::BinaryHandshake;
use crate::daemon::LegacyDaemonHandshake;
use crate::session::handshake::SessionHandshake;
use protocol::NegotiationPrologue;

use super::helpers::{create_binary_handshake, create_legacy_handshake};

#[test]
fn stream_returns_reference_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let stream = session.stream();
    assert_eq!(stream.decision(), NegotiationPrologue::Binary);
}

#[test]
fn stream_returns_reference_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let stream = session.stream();
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
}

#[test]
fn stream_mut_returns_mutable_reference_binary() {
    let binary = create_binary_handshake();
    let mut session = SessionHandshake::Binary(binary);
    let stream = session.stream_mut();
    assert_eq!(stream.decision(), NegotiationPrologue::Binary);
}

#[test]
fn stream_mut_returns_mutable_reference_legacy() {
    let legacy = create_legacy_handshake();
    let mut session = SessionHandshake::Legacy(legacy);
    let stream = session.stream_mut();
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
}

#[test]
fn into_stream_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let stream = session.into_stream();
    assert_eq!(stream.decision(), NegotiationPrologue::Binary);
}

#[test]
fn into_stream_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let stream = session.into_stream();
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
}

#[test]
fn as_binary_returns_some_for_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(session.as_binary().is_some());
}

#[test]
fn as_binary_returns_none_for_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(session.as_binary().is_none());
}

#[test]
fn as_binary_mut_returns_some_for_binary() {
    let binary = create_binary_handshake();
    let mut session = SessionHandshake::Binary(binary);
    assert!(session.as_binary_mut().is_some());
}

#[test]
fn as_binary_mut_returns_none_for_legacy() {
    let legacy = create_legacy_handshake();
    let mut session = SessionHandshake::Legacy(legacy);
    assert!(session.as_binary_mut().is_none());
}

#[test]
fn as_legacy_returns_some_for_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(session.as_legacy().is_some());
}

#[test]
fn as_legacy_returns_none_for_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(session.as_legacy().is_none());
}

#[test]
fn as_legacy_mut_returns_some_for_legacy() {
    let legacy = create_legacy_handshake();
    let mut session = SessionHandshake::Legacy(legacy);
    assert!(session.as_legacy_mut().is_some());
}

#[test]
fn as_legacy_mut_returns_none_for_binary() {
    let binary = create_binary_handshake();
    let mut session = SessionHandshake::Binary(binary);
    assert!(session.as_legacy_mut().is_none());
}

#[test]
fn into_binary_ok_for_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(session.into_binary().is_ok());
}

#[test]
fn into_binary_err_for_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(session.into_binary().is_err());
}

#[test]
fn into_legacy_ok_for_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    assert!(session.into_legacy().is_ok());
}

#[test]
fn into_legacy_err_for_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    assert!(session.into_legacy().is_err());
}

#[test]
fn from_binary_handshake() {
    let binary = create_binary_handshake();
    let session: SessionHandshake<_> = binary.into();
    assert!(session.is_binary());
}

#[test]
fn from_legacy_handshake() {
    let legacy = create_legacy_handshake();
    let session: SessionHandshake<_> = legacy.into();
    assert!(session.is_legacy());
}

#[test]
fn try_from_session_to_binary_ok() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let result: Result<BinaryHandshake<_>, _> = session.try_into();
    assert!(result.is_ok());
}

#[test]
fn try_from_session_to_binary_err() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let result: Result<BinaryHandshake<_>, _> = session.try_into();
    assert!(result.is_err());
}

#[test]
fn try_from_session_to_legacy_ok() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let result: Result<LegacyDaemonHandshake<_>, _> = session.try_into();
    assert!(result.is_ok());
}

#[test]
fn try_from_session_to_legacy_err() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let result: Result<LegacyDaemonHandshake<_>, _> = session.try_into();
    assert!(result.is_err());
}

#[test]
fn map_stream_inner_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let mapped = session.map_stream_inner(|_cursor| {});
    assert!(mapped.is_binary());
}

#[test]
fn map_stream_inner_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let mapped = session.map_stream_inner(|_cursor| {});
    assert!(mapped.is_legacy());
}

#[test]
fn into_inner_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let _inner: std::io::Cursor<Vec<u8>> = session.into_inner();
}

#[test]
fn into_inner_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let _inner: std::io::Cursor<Vec<u8>> = session.into_inner();
}
