//! Golden byte tests for the `MSG_DELETED` (tag 101) multiplex frame.
//!
//! A server generator forwards each `--delete` victim to the client as a
//! `MSG_DELETED` frame carrying the raw deletion-root-relative name; a directory
//! carries a trailing NUL so the reader can tell it from a regular file. These
//! tests pin the exact wire bytes (4-byte LE header + payload) so the encoding
//! stays byte-compatible with upstream rsync.
//!
//! # Upstream Reference
//!
//! - `rsync.h:300` - `MSG_DELETED = 101`.
//! - `io.c:mplex_write()` - header = `(MPLEX_BASE + code) << 24 | len`, so the
//!   tag byte is `7 + 101 = 108 = 0x6C`.
//! - `log.c:866-869` - `send_msg(MSG_DELETED, fname, len, ...)`; a directory
//!   bumps `len` to include its trailing NUL (`log.c:867-868`).
//! - `io.c:1616` - the reader treats a trailing NUL as a directory marker.

use std::io::Cursor;

use protocol::{MessageCode, MessageFrame, MessageHeader, recv_msg_into, send_msg};

#[test]
fn golden_msg_deleted_header_tag_is_108() {
    // Tag = MPLEX_BASE(7) + 101 = 108 = 0x6C. For payload length 3:
    // header = (108 << 24) | 3 = 0x6C000003, LE bytes [0x03, 0x00, 0x00, 0x6C].
    let header = MessageHeader::new(MessageCode::Deleted, 3).unwrap();
    assert_eq!(header.encode(), [0x03, 0x00, 0x00, 0x6C]);

    let decoded = MessageHeader::decode(&[0x03, 0x00, 0x00, 0x6C]).unwrap();
    assert_eq!(decoded.code(), MessageCode::Deleted);
    assert_eq!(decoded.payload_len(), 3);
}

#[test]
fn golden_msg_deleted_file_frame_exact_bytes() {
    // A deleted regular file "foo": payload is the raw name, no trailing NUL.
    // len = 3, header 0x6C000003 (LE [0x03,0x00,0x00,0x6C]), then b"foo".
    let frame = MessageFrame::new(MessageCode::Deleted, b"foo".to_vec()).unwrap();
    let mut bytes = Vec::new();
    frame.encode_into_vec(&mut bytes).unwrap();

    assert_eq!(bytes, [0x03, 0x00, 0x00, 0x6C, b'f', b'o', b'o']);
}

#[test]
fn golden_msg_deleted_dir_frame_has_trailing_nul() {
    // A deleted directory "bar": upstream bumps len to include the trailing NUL
    // (log.c:867-868), so the payload is b"bar\0" (len 4). header 0x6C000004,
    // LE [0x04,0x00,0x00,0x6C], then b"bar\0".
    let frame = MessageFrame::new(MessageCode::Deleted, b"bar\0".to_vec()).unwrap();
    let mut bytes = Vec::new();
    frame.encode_into_vec(&mut bytes).unwrap();

    assert_eq!(bytes, [0x04, 0x00, 0x00, 0x6C, b'b', b'a', b'r', 0x00]);
}

#[test]
fn golden_msg_deleted_send_recv_round_trip() {
    // send_msg writes the same frame the receiver reads back verbatim, so the
    // client's demux sees the exact payload (dir marker preserved).
    let mut wire = Vec::new();
    send_msg(&mut wire, MessageCode::Deleted, b"sub/dir\0").unwrap();
    assert_eq!(wire[..4], [0x08, 0x00, 0x00, 0x6C]);

    let mut reader = Cursor::new(wire);
    let mut buf = Vec::new();
    let code = recv_msg_into(&mut reader, &mut buf).unwrap();
    assert_eq!(code, MessageCode::Deleted);
    assert_eq!(buf, b"sub/dir\0");
    // Trailing NUL is the directory marker the client strips before rendering.
    assert_eq!(buf.last(), Some(&0u8));
}
