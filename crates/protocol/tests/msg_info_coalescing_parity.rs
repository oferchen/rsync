//! Wire-byte parity regression tests for MSG_INFO frame coalescing.
//!
//! MIF-5 introduced coalescing in the multiplex writer: consecutive MSG_INFO
//! (and MSG_WARNING) frames skip the immediate flush, accumulating in the write
//! buffer until the next explicit flush or DATA write drains them. This reduces
//! TCP segment count but must not alter the logical content the receiver sees.
//!
//! These tests verify that coalescing is a transparent optimization - the reader
//! recovers byte-identical logical messages regardless of physical framing.

use std::io::{self, Cursor, Read, Write};
use std::sync::{Arc, Mutex};

use protocol::{MessageCode, MplexReader, MplexWriter, recv_msg, send_msg};

// ---------------------------------------------------------------------------
// Helper: drain all frames from a wire buffer via recv_msg, returning
// (code, payload) pairs. Stops at EOF.
// ---------------------------------------------------------------------------
fn drain_frames(wire: &[u8]) -> Vec<(MessageCode, Vec<u8>)> {
    let mut cursor = Cursor::new(wire);
    let mut frames = Vec::new();
    loop {
        match recv_msg(&mut cursor) {
            Ok(frame) => frames.push((frame.code(), frame.payload().to_vec())),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("unexpected recv_msg error: {e}"),
        }
    }
    frames
}

// ---------------------------------------------------------------------------
// Helper: drain MplexReader, collecting DATA bytes and out-of-band messages.
// ---------------------------------------------------------------------------
fn drain_mplex(wire: &[u8]) -> (Vec<u8>, Vec<(MessageCode, Vec<u8>)>) {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let messages_clone = messages.clone();

    let mut reader = MplexReader::new(Cursor::new(wire));
    reader.set_message_handler(move |code, payload| {
        messages_clone
            .lock()
            .unwrap()
            .push((code, payload.to_vec()));
    });

    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("unexpected MplexReader error: {e}"),
        }
    }

    let oob = Arc::try_unwrap(messages).unwrap().into_inner().unwrap();
    (data, oob)
}

// ---------------------------------------------------------------------------
// Core parity: consecutive MSG_INFO frames via MplexWriter produce wire bytes
// identical to individual send_msg calls (the pre-coalescing behavior).
// ---------------------------------------------------------------------------
#[test]
fn consecutive_msg_info_wire_bytes_identical_to_individual_sends() {
    let payloads: &[&[u8]] = &[
        b"file1.txt\n",
        b"file2.txt\n",
        b"file3.txt\n",
        b"file4.txt\n",
        b"file5.txt\n",
    ];

    // Coalesced path: MplexWriter defers flush for MSG_INFO.
    let mut coalesced = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut coalesced);
        for payload in payloads {
            writer.write_message(MessageCode::Info, payload).unwrap();
        }
        writer.flush().unwrap();
    }

    // Reference path: direct send_msg per frame (no coalescing).
    let mut reference = Vec::new();
    for payload in payloads {
        send_msg(&mut reference, MessageCode::Info, payload).unwrap();
    }

    assert_eq!(
        coalesced, reference,
        "coalesced wire bytes must be identical to individual send_msg calls"
    );
}

// ---------------------------------------------------------------------------
// Logical parity via recv_msg: decoded frames are identical.
// ---------------------------------------------------------------------------
#[test]
fn coalesced_frames_decode_identically_via_recv_msg() {
    let payloads: &[&[u8]] = &[b"alpha\n", b"bravo\n", b"charlie\n"];

    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        for payload in payloads {
            writer.write_message(MessageCode::Info, payload).unwrap();
        }
        writer.flush().unwrap();
    }

    let frames = drain_frames(&wire);
    assert_eq!(frames.len(), payloads.len());
    for (i, payload) in payloads.iter().enumerate() {
        assert_eq!(frames[i].0, MessageCode::Info, "frame {i} code mismatch");
        assert_eq!(frames[i].1, *payload, "frame {i} payload mismatch");
    }
}

// ---------------------------------------------------------------------------
// Logical parity via MplexReader: out-of-band handler receives identical
// messages regardless of coalescing.
// ---------------------------------------------------------------------------
#[test]
fn coalesced_info_visible_through_mplex_reader_handler() {
    let payloads: &[&[u8]] = &[b"progress 1/3\n", b"progress 2/3\n", b"progress 3/3\n"];

    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        for payload in payloads {
            writer.write_message(MessageCode::Info, payload).unwrap();
        }
        // Append a DATA frame so MplexReader has something to return.
        writer.write_all(b"done").unwrap();
        writer.flush().unwrap();
    }

    let (data, oob) = drain_mplex(&wire);
    assert_eq!(data, b"done");
    assert_eq!(oob.len(), payloads.len());
    for (i, payload) in payloads.iter().enumerate() {
        assert_eq!(oob[i].0, MessageCode::Info, "oob {i} code mismatch");
        assert_eq!(oob[i].1, *payload, "oob {i} payload mismatch");
    }
}

// ---------------------------------------------------------------------------
// Single frame: no coalescing opportunity. Must still work.
// ---------------------------------------------------------------------------
#[test]
fn single_msg_info_no_coalescing_opportunity() {
    let mut coalesced = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut coalesced);
        writer
            .write_message(MessageCode::Info, b"only one\n")
            .unwrap();
        writer.flush().unwrap();
    }

    let mut reference = Vec::new();
    send_msg(&mut reference, MessageCode::Info, b"only one\n").unwrap();

    assert_eq!(coalesced, reference);

    let frames = drain_frames(&coalesced);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, MessageCode::Info);
    assert_eq!(frames[0].1, b"only one\n");
}

// ---------------------------------------------------------------------------
// Mixed MSG_INFO + MSG_DATA: coalescing must not cross message types.
// Frame ordering must be preserved.
// ---------------------------------------------------------------------------
#[test]
fn mixed_info_and_data_preserves_ordering() {
    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        writer
            .write_message(MessageCode::Info, b"info-before\n")
            .unwrap();
        writer.write_all(b"chunk1").unwrap();
        writer
            .write_message(MessageCode::Info, b"info-middle\n")
            .unwrap();
        writer.write_all(b"chunk2").unwrap();
        writer
            .write_message(MessageCode::Info, b"info-after\n")
            .unwrap();
        writer.flush().unwrap();
    }

    // Verify frame-level ordering via recv_msg.
    let frames = drain_frames(&wire);
    let expected: &[(MessageCode, &[u8])] = &[
        (MessageCode::Info, b"info-before\n"),
        (MessageCode::Data, b"chunk1"),
        (MessageCode::Info, b"info-middle\n"),
        (MessageCode::Data, b"chunk2"),
        (MessageCode::Info, b"info-after\n"),
    ];
    assert_eq!(frames.len(), expected.len(), "frame count mismatch");
    for (i, (code, payload)) in expected.iter().enumerate() {
        assert_eq!(frames[i].0, *code, "frame {i} code");
        assert_eq!(frames[i].1, *payload, "frame {i} payload");
    }
}

// ---------------------------------------------------------------------------
// Mixed MSG_INFO + MSG_WARNING: both are batchable, both must survive.
// ---------------------------------------------------------------------------
#[test]
fn mixed_info_and_warning_both_batchable() {
    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        writer
            .write_message(MessageCode::Info, b"info line\n")
            .unwrap();
        writer
            .write_message(MessageCode::Warning, b"warn line\n")
            .unwrap();
        writer
            .write_message(MessageCode::Info, b"info again\n")
            .unwrap();
        writer.flush().unwrap();
    }

    let frames = drain_frames(&wire);
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], (MessageCode::Info, b"info line\n".to_vec()));
    assert_eq!(frames[1], (MessageCode::Warning, b"warn line\n".to_vec()));
    assert_eq!(frames[2], (MessageCode::Info, b"info again\n".to_vec()));
}

// ---------------------------------------------------------------------------
// Empty payloads: coalescing must handle zero-length MSG_INFO correctly.
// ---------------------------------------------------------------------------
#[test]
fn empty_payload_msg_info_coalesces_correctly() {
    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        writer.write_message(MessageCode::Info, b"").unwrap();
        writer
            .write_message(MessageCode::Info, b"non-empty\n")
            .unwrap();
        writer.write_message(MessageCode::Info, b"").unwrap();
        writer.flush().unwrap();
    }

    let frames = drain_frames(&wire);
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], (MessageCode::Info, Vec::new()));
    assert_eq!(frames[1], (MessageCode::Info, b"non-empty\n".to_vec()));
    assert_eq!(frames[2], (MessageCode::Info, Vec::new()));

    // Wire-byte parity with direct sends.
    let mut reference = Vec::new();
    send_msg(&mut reference, MessageCode::Info, b"").unwrap();
    send_msg(&mut reference, MessageCode::Info, b"non-empty\n").unwrap();
    send_msg(&mut reference, MessageCode::Info, b"").unwrap();

    assert_eq!(wire, reference);
}

// ---------------------------------------------------------------------------
// Latency-sensitive code interrupting a batch: MSG_ERROR must flush, so
// preceding MSG_INFO frames appear before the error in the wire stream.
// ---------------------------------------------------------------------------
#[test]
fn latency_sensitive_code_flushes_preceding_info() {
    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        writer
            .write_message(MessageCode::Info, b"before error\n")
            .unwrap();
        writer
            .write_message(MessageCode::Error, b"fatal\n")
            .unwrap();
        writer
            .write_message(MessageCode::Info, b"after error\n")
            .unwrap();
        writer.flush().unwrap();
    }

    let frames = drain_frames(&wire);
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].0, MessageCode::Info);
    assert_eq!(frames[0].1, b"before error\n");
    assert_eq!(frames[1].0, MessageCode::Error);
    assert_eq!(frames[1].1, b"fatal\n");
    assert_eq!(frames[2].0, MessageCode::Info);
    assert_eq!(frames[2].1, b"after error\n");
}

// ---------------------------------------------------------------------------
// Typical multi-file transfer pattern: many MSG_INFO lines interleaved with
// DATA frames, verified end-to-end through MplexReader.
// ---------------------------------------------------------------------------
#[test]
fn realistic_multi_file_transfer_pattern() {
    let file_count = 20;

    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        for i in 0..file_count {
            let info = format!("file_{i:04}.dat\n");
            writer
                .write_message(MessageCode::Info, info.as_bytes())
                .unwrap();
            // Simulate file data for each file.
            let data = vec![(i & 0xFF) as u8; 128];
            writer.write_all(&data).unwrap();
        }
        writer.flush().unwrap();
    }

    let (data, oob) = drain_mplex(&wire);

    // Verify all info messages arrived in order.
    assert_eq!(oob.len(), file_count);
    for (i, (code, payload)) in oob.iter().enumerate() {
        assert_eq!(*code, MessageCode::Info);
        let expected = format!("file_{i:04}.dat\n");
        assert_eq!(
            payload,
            expected.as_bytes(),
            "info message {i} content mismatch"
        );
    }

    // Verify all data bytes arrived (order is preserved by MplexReader).
    let mut expected_data = Vec::new();
    for i in 0..file_count {
        expected_data.extend_from_slice(&vec![(i & 0xFF) as u8; 128]);
    }
    assert_eq!(data, expected_data);
}

// ---------------------------------------------------------------------------
// MSG_WARNING coalescing has the same wire-byte parity as MSG_INFO.
// ---------------------------------------------------------------------------
#[test]
fn msg_warning_coalescing_wire_byte_parity() {
    let payloads: &[&[u8]] = &[
        b"slow network\n",
        b"partial transfer\n",
        b"retrying\n",
    ];

    let mut coalesced = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut coalesced);
        for payload in payloads {
            writer
                .write_message(MessageCode::Warning, payload)
                .unwrap();
        }
        writer.flush().unwrap();
    }

    let mut reference = Vec::new();
    for payload in payloads {
        send_msg(&mut reference, MessageCode::Warning, payload).unwrap();
    }

    assert_eq!(
        coalesced, reference,
        "MSG_WARNING coalesced wire bytes must match individual sends"
    );
}

// ---------------------------------------------------------------------------
// Data-then-info ordering: buffered DATA must be flushed before the control
// message regardless of coalescing behavior.
// ---------------------------------------------------------------------------
#[test]
fn buffered_data_flushed_before_info_message() {
    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        writer.write_all(b"pending data").unwrap();
        writer
            .write_message(MessageCode::Info, b"status\n")
            .unwrap();
        writer.flush().unwrap();
    }

    let frames = drain_frames(&wire);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].0, MessageCode::Data);
    assert_eq!(frames[0].1, b"pending data");
    assert_eq!(frames[1].0, MessageCode::Info);
    assert_eq!(frames[1].1, b"status\n");
}

// ---------------------------------------------------------------------------
// Many small MSG_INFO frames: stress test that coalescing handles a large
// batch without losing any frames.
// ---------------------------------------------------------------------------
#[test]
fn many_small_info_frames_none_lost() {
    let count = 200;

    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);
        for i in 0..count {
            let msg = format!("{i}\n");
            writer
                .write_message(MessageCode::Info, msg.as_bytes())
                .unwrap();
        }
        writer.flush().unwrap();
    }

    let frames = drain_frames(&wire);
    assert_eq!(frames.len(), count, "expected {count} frames, got {}", frames.len());
    for (i, (code, payload)) in frames.iter().enumerate() {
        assert_eq!(*code, MessageCode::Info);
        let expected = format!("{i}\n");
        assert_eq!(payload, expected.as_bytes(), "frame {i} payload mismatch");
    }

    // Wire-byte parity.
    let mut reference = Vec::new();
    for i in 0..count {
        let msg = format!("{i}\n");
        send_msg(&mut reference, MessageCode::Info, msg.as_bytes()).unwrap();
    }
    assert_eq!(wire, reference);
}

// ---------------------------------------------------------------------------
// Coalescing with write_info convenience method produces same output as
// write_message with MessageCode::Info.
// ---------------------------------------------------------------------------
#[test]
fn write_info_convenience_matches_write_message() {
    let mut via_convenience = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut via_convenience);
        writer.write_info("file_a.txt\n").unwrap();
        writer.write_info("file_b.txt\n").unwrap();
        writer.flush().unwrap();
    }

    let mut via_write_message = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut via_write_message);
        writer
            .write_message(MessageCode::Info, b"file_a.txt\n")
            .unwrap();
        writer
            .write_message(MessageCode::Info, b"file_b.txt\n")
            .unwrap();
        writer.flush().unwrap();
    }

    assert_eq!(via_convenience, via_write_message);
}
