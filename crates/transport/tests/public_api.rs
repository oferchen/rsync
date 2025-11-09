use protocol::ProtocolVersion;
use std::io::Cursor;
use transport::{
    BufferedCopyTooSmall, NegotiatedStreamParts, SessionHandshakeParts, local_cap_reduced_protocol,
};

fn assert_type_visible<T>() {}

#[test]
fn session_handshake_parts_is_publicly_visible() {
    assert_type_visible::<SessionHandshakeParts<Cursor<Vec<u8>>>>();
}

#[test]
fn negotiated_stream_parts_remains_accessible() {
    assert_type_visible::<NegotiatedStreamParts<Cursor<Vec<u8>>>>();
}

#[test]
fn buffered_copy_too_small_is_publicly_visible() {
    assert_type_visible::<BufferedCopyTooSmall>();
}

#[test]
fn local_cap_helper_is_public() {
    assert!(local_cap_reduced_protocol(
        ProtocolVersion::V31,
        ProtocolVersion::V29,
    ));
}
