use rsync_transport::{NegotiatedStreamParts, SessionHandshakeParts};
use std::io::Cursor;

fn assert_type_visible<T>() {}

#[test]
fn session_handshake_parts_is_publicly_visible() {
    assert_type_visible::<SessionHandshakeParts<Cursor<Vec<u8>>>>();
}

#[test]
fn negotiated_stream_parts_remains_accessible() {
    assert_type_visible::<NegotiatedStreamParts<Cursor<Vec<u8>>>>();
}
