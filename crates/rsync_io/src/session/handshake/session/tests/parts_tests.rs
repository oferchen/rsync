//! Tests for parts decomposition and reassembly.

use crate::session::handshake::SessionHandshake;
use crate::session::parts::SessionHandshakeParts;

use super::helpers::{create_binary_handshake, create_legacy_handshake};

#[test]
fn into_parts_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let parts = session.into_parts();
    assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
}

#[test]
fn into_parts_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let parts = session.into_parts();
    assert!(matches!(parts, SessionHandshakeParts::Legacy(_)));
}

#[test]
fn into_stream_parts_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let parts = session.into_stream_parts();
    assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
}

#[test]
fn into_stream_parts_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let parts = session.into_stream_parts();
    assert!(matches!(parts, SessionHandshakeParts::Legacy(_)));
}

#[test]
fn from_parts_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let parts = session.into_parts();
    let rebuilt = SessionHandshake::from_parts(parts);
    assert!(rebuilt.is_binary());
}

#[test]
fn from_parts_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let parts = session.into_parts();
    let rebuilt = SessionHandshake::from_parts(parts);
    assert!(rebuilt.is_legacy());
}

#[test]
fn from_stream_parts_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let parts = session.into_stream_parts();
    let rebuilt = SessionHandshake::from_stream_parts(parts);
    assert!(rebuilt.is_binary());
}

#[test]
fn from_stream_parts_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let parts = session.into_stream_parts();
    let rebuilt = SessionHandshake::from_stream_parts(parts);
    assert!(rebuilt.is_legacy());
}

#[test]
fn parts_into_session_binary() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let parts = session.into_parts();
    let rebuilt: SessionHandshake<_> = parts.into();
    assert!(rebuilt.is_binary());
}

#[test]
fn parts_into_session_legacy() {
    let legacy = create_legacy_handshake();
    let session = SessionHandshake::Legacy(legacy);
    let parts = session.into_parts();
    let rebuilt: SessionHandshake<_> = parts.into();
    assert!(rebuilt.is_legacy());
}

#[test]
fn session_into_parts() {
    let binary = create_binary_handshake();
    let session = SessionHandshake::Binary(binary);
    let parts: SessionHandshakeParts<_> = session.into();
    assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
}
