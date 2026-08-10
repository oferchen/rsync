//! Regression tests for the `-X --fake-super` finalize wire-protocol fix.
//!
//! Locks in the sender-side handling of the generator's `send_xattr_request()`
//! stream when `ITEM_REPORT_XATTR` is set in iflags. Before the fix, the
//! sender skipped the request body and read the subsequent `sum_head`
//! starting from the request terminator byte, desyncing the wire stream and
//! aborting the goodbye phase with "block length must be non-zero" or
//! "Invalid remainder length" errors under `-X --fake-super`.
//!
//! # Upstream Reference
//!
//! - `sender.c:280-284` - `recv_xattr_request()` on the sender after iflags
//! - `sender.c:192-196` - `send_xattr_request()` in `write_ndx_and_attrs()`
//! - `xattrs.c:623-675` - sender path emits `rel_num + len + data + 0`
//! - `generator.c:585-592` - generator emits at least the 0 terminator when
//!   `ITEM_REPORT_XATTR` is set on a new file (count mismatch path).

use std::io::Cursor;

use protocol::xattr::{XattrEntry, XattrList, XattrState, send_sender_xattr_response};
use protocol::{read_varint, write_varint};

/// Mirrors the upstream generator's `send_xattr_request(NULL, file, f_out)`
/// wire emission for a file with no `XSTATE_TODO` entries: a single 0-byte
/// varint terminator. This is the byte that desynchronised the transfer
/// under `-X --fake-super` when xattr counts differed (e.g. source has
/// `user.foo` but the brand-new destination has none yet).
fn write_generator_request_terminator(buf: &mut Vec<u8>) {
    write_varint(buf, 0).unwrap();
}

/// Mirrors the upstream generator's request with one TODO index. Used to
/// cover the case where the abbreviated checksum diff flags one entry that
/// the sender must resend in full.
fn write_generator_request_with_indices(buf: &mut Vec<u8>, indices: &[i32]) {
    let mut prior = 0i32;
    for &num in indices {
        write_varint(buf, num - prior).unwrap();
        prior = num;
    }
    write_varint(buf, 0).unwrap();
}

/// Sender consumes the generator's bare-terminator request: zero entries
/// flagged TODO, then re-emits its own bare-terminator response. Without
/// this read/emit pair the receiver picks up the terminator as the first
/// byte of `sum_head.count` and aborts with "Invalid remainder length".
#[test]
fn sender_consumes_generator_bare_terminator() {
    let mut request_stream = Vec::new();
    write_generator_request_terminator(&mut request_stream);

    let mut list = XattrList::new();
    list.push(XattrEntry::new(b"user.foo".to_vec(), b"bar".to_vec()));
    list.entries_mut()[0].set_num(1);

    let mut cursor = Cursor::new(request_stream);
    let indices = protocol::xattr::recv_xattr_request(&mut cursor, &mut list).unwrap();

    assert!(indices.is_empty(), "no entries should be flagged TODO");
    assert_eq!(list.entries()[0].state(), XattrState::Done);

    // Sender's echo: only the terminator, matching upstream's bare 0.
    let mut response_stream = Vec::new();
    send_sender_xattr_response(&mut response_stream, &mut list).unwrap();
    assert_eq!(response_stream, vec![0u8]);
}

/// End-to-end sender-side path: generator requests indices 1 and 3, the
/// sender marks the corresponding entries TODO and re-emits them with full
/// values plus the 0 terminator. The receiver's
/// `read_xattr_abbreviation_data` (transfer crate, mirrored here as a
/// hand-rolled decoder) recovers the (num, value) pairs.
#[test]
fn sender_round_trips_request_into_response() {
    let value1 = vec![0x11u8; 64];
    let value3 = vec![0x33u8; 96];
    let middle = vec![0x22u8; 8];

    // Generator requests entries 1 and 3 (skipping 2). Wire stream is
    // delta-encoded 1-based nums terminated by 0.
    let mut request_stream = Vec::new();
    write_generator_request_with_indices(&mut request_stream, &[1, 3]);

    // Sender's local xattr list for the file - all values are present in
    // full. The receiver flags entries 1 and 3 as needing full values via
    // the request stream.
    let mut sender_list = XattrList::new();
    let mut e1 = XattrEntry::new(b"user.a".to_vec(), value1.clone());
    e1.set_num(1);
    sender_list.push(e1);

    let mut e2 = XattrEntry::new(b"user.b".to_vec(), middle.clone());
    e2.set_num(2);
    sender_list.push(e2);

    let mut e3 = XattrEntry::new(b"user.c".to_vec(), value3.clone());
    e3.set_num(3);
    sender_list.push(e3);

    // Sender reads the request, marking the requested entries TODO.
    let mut request_cursor = Cursor::new(request_stream);
    let requested =
        protocol::xattr::recv_xattr_request(&mut request_cursor, &mut sender_list).unwrap();
    assert_eq!(requested, vec![0usize, 2usize]); // 0-based indices for nums 1 and 3
    assert_eq!(sender_list.entries()[0].state(), XattrState::Todo);
    assert_eq!(sender_list.entries()[1].state(), XattrState::Done);
    assert_eq!(sender_list.entries()[2].state(), XattrState::Todo);

    // Sender emits the response.
    let mut response_stream = Vec::new();
    send_sender_xattr_response(&mut response_stream, &mut sender_list).unwrap();

    // Receiver decodes the response - mirrors transfer crate's
    // read_xattr_abbreviation_data layout exactly.
    let mut cursor = Cursor::new(response_stream);
    let mut prior = 0i32;
    let mut decoded = Vec::new();
    loop {
        let rel = read_varint(&mut cursor).unwrap();
        if rel == 0 {
            break;
        }
        let num = prior + rel;
        prior = num;
        let len = read_varint(&mut cursor).unwrap() as usize;
        let mut data = vec![0u8; len];
        use std::io::Read;
        cursor.read_exact(&mut data).unwrap();
        decoded.push((num, data));
    }

    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0], (1, value1));
    assert_eq!(decoded[1], (3, value3));

    // States reset to Done after emission (upstream xattrs.c:651).
    assert_eq!(sender_list.entries()[0].state(), XattrState::Done);
    assert_eq!(sender_list.entries()[2].state(), XattrState::Done);
}

/// Confirms the wire byte sequence matches upstream exactly. The bytes
/// emitted for "two entries, nums 1 and 3, both values of length 5" are:
///   rel_num=1, len=5, "AAAAA", rel_num=2, len=5, "BBBBB", 0
/// All varints in this range are single bytes.
#[test]
fn wire_byte_layout_matches_upstream() {
    let mut list = XattrList::new();
    let mut e1 = XattrEntry::new(b"user.a".to_vec(), b"AAAAA".to_vec());
    e1.set_num(1);
    e1.mark_todo();
    list.push(e1);

    let mut e2 = XattrEntry::new(b"user.b".to_vec(), b"BBBBB".to_vec());
    e2.set_num(3);
    e2.mark_todo();
    list.push(e2);

    let mut buf = Vec::new();
    send_sender_xattr_response(&mut buf, &mut list).unwrap();

    // Expected layout:
    //   varint(1)  = 0x01   // rel_num for first entry
    //   varint(5)  = 0x05   // length
    //   "AAAAA"             // 5 bytes
    //   varint(2)  = 0x02   // rel_num = 3 - 1 = 2
    //   varint(5)  = 0x05   // length
    //   "BBBBB"             // 5 bytes
    //   varint(0)  = 0x00   // terminator
    let expected = vec![
        0x01, 0x05, b'A', b'A', b'A', b'A', b'A', 0x02, 0x05, b'B', b'B', b'B', b'B', b'B', 0x00,
    ];
    assert_eq!(buf, expected);
}
