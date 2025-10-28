use super::{
    BorrowedMessageFrame, BorrowedMessageFrames, MessageFrame,
    helpers::{ensure_payload_length, reserve_payload},
    recv_msg, recv_msg_into, send_frame, send_msg,
};
use crate::envelope::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MPLEX_BASE, MessageCode, MessageHeader};
use std::collections::VecDeque;
use std::convert::TryFrom as _;
use std::io::{self, IoSlice, Write};
use std::slice;

fn encode_frame(code: MessageCode, payload: &[u8]) -> Vec<u8> {
    let header = MessageHeader::new(code, payload.len() as u32).expect("constructible header");
    let mut bytes = Vec::from(header.encode());
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn send_and_receive_round_trip_info_message() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Info, b"hello world").expect("send succeeds");

    let mut cursor = io::Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"hello world");
    assert_eq!(frame.payload_len(), b"hello world".len());
}

#[test]
fn send_msg_emits_upstream_compatible_envelope_for_common_tags() {
    struct Sample {
        code: MessageCode,
        payload: &'static [u8],
    }

    let samples = [
        Sample {
            code: MessageCode::Info,
            payload: b"abc",
        },
        Sample {
            code: MessageCode::Error,
            payload: b"",
        },
        Sample {
            code: MessageCode::Stats,
            payload: &[0x01, 0x02, 0x03, 0x04],
        },
    ];

    for sample in samples {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, sample.code, sample.payload).expect("send succeeds");

        assert_eq!(buffer.len(), HEADER_LEN + sample.payload.len());

        // Upstream encodes multiplexed headers using the MPLEX_BASE constant, adds the message
        // code, shifts the tag into the high byte, and stores the payload length in
        // little-endian order. See rsync 3.4.1's io.c:send_msg().
        let tag = u32::from(MPLEX_BASE) + u32::from(sample.code.as_u8());
        let expected_header = ((tag << 24) | sample.payload.len() as u32).to_le_bytes();

        assert_eq!(&buffer[..HEADER_LEN], &expected_header);
        assert_eq!(&buffer[HEADER_LEN..], sample.payload);
    }
}

#[test]
fn send_msg_prefers_vectored_writes_when_supported() {
    struct RecordingWriter {
        writes: Vec<u8>,
        write_calls: usize,
        vectored_calls: usize,
    }

    impl RecordingWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                write_calls: 0,
                vectored_calls: 0,
            }
        }
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_calls += 1;
            let mut written = 0;
            for buf in bufs {
                self.writes.extend_from_slice(buf);
                written += buf.len();
            }
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = RecordingWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Warning, payload).expect("send succeeds");

    assert_eq!(writer.write_calls, 0, "fallback write() should not be used");
    assert_eq!(writer.vectored_calls, 1, "single vectored call expected");

    let header = MessageHeader::new(MessageCode::Warning, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_msg_falls_back_when_vectored_is_not_supported() {
    struct NoVectoredWriter {
        writes: Vec<u8>,
        write_calls: usize,
        vectored_attempts: usize,
    }

    impl NoVectoredWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                write_calls: 0,
                vectored_attempts: 0,
            }
        }
    }

    impl Write for NoVectoredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_attempts += 1;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored IO disabled",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = NoVectoredWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Warning, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Warning, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);

    assert_eq!(writer.vectored_attempts, 1);
    assert_eq!(writer.write_calls, 2);
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_msg_falls_back_after_vectored_reports_unsupported() {
    struct UnsupportedVectoredWriter {
        writes: Vec<u8>,
        vectored_attempts: usize,
        sequential_calls: usize,
    }

    impl UnsupportedVectoredWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                vectored_attempts: 0,
                sequential_calls: 0,
            }
        }
    }

    impl Write for UnsupportedVectoredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.sequential_calls += 1;
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_attempts += 1;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored IO disabled",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = UnsupportedVectoredWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Info, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);

    assert_eq!(
        writer.vectored_attempts, 1,
        "one vectored attempt should occur before fallback"
    );
    assert!(
        writer.sequential_calls >= 2,
        "fallback must write header and payload sequentially"
    );
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_msg_handles_partial_vectored_writes() {
    struct PartialWriter {
        schedule: VecDeque<usize>,
        written: Vec<u8>,
        write_calls: usize,
    }

    impl PartialWriter {
        fn new(schedule: VecDeque<usize>) -> Self {
            Self {
                schedule,
                written: Vec::new(),
                write_calls: 0,
            }
        }

        fn record(&mut self, mut remaining: usize, bufs: &[IoSlice<'_>]) -> usize {
            let mut produced = 0usize;

            for buf in bufs {
                if remaining == 0 {
                    break;
                }
                if buf.is_empty() {
                    continue;
                }

                let take = buf.len().min(remaining);
                self.written.extend_from_slice(&buf[..take]);
                produced += take;
                remaining -= take;

                if take < buf.len() {
                    break;
                }
            }

            produced
        }
    }

    impl Write for PartialWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_vectored(slice::from_ref(&IoSlice::new(buf)))
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.write_calls += 1;

            let allowed = self.schedule.pop_front().unwrap_or(usize::MAX);
            debug_assert!(
                allowed != 0,
                "partial writer schedule must contain positive chunk sizes",
            );
            if allowed == 0 {
                return Ok(0);
            }

            let produced = self.record(allowed, bufs);
            if produced == 0 {
                return Ok(0);
            }

            Ok(produced)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = PartialWriter::new(VecDeque::from(vec![2, 1, 3, 2]));
    let payload = b"chunked-payload";
    send_msg(&mut writer, MessageCode::Info, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);

    assert_eq!(writer.written, expected);
    assert!(
        writer.write_calls >= 4,
        "partial schedule should trigger repeated writes"
    );
}

#[test]
fn decode_from_slice_round_trips_and_exposes_remainder() {
    let first = encode_frame(MessageCode::Info, b"hello");
    let second = encode_frame(MessageCode::Error, b"world");

    let mut concatenated = first.clone();
    concatenated.extend_from_slice(&second);

    let (frame, remainder) =
        MessageFrame::decode_from_slice(&concatenated).expect("decode succeeds");
    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"hello");
    assert_eq!(remainder, second.as_slice());
}

#[test]
fn borrowed_message_frame_decodes_without_allocating_payload() {
    let first = encode_frame(MessageCode::Data, b"abcde");
    let second = encode_frame(MessageCode::Info, b"more");

    let mut concatenated = first.clone();
    concatenated.extend_from_slice(&second);

    let (frame, remainder) =
        BorrowedMessageFrame::decode_from_slice(&concatenated).expect("decode succeeds");
    assert_eq!(frame.code(), MessageCode::Data);
    assert_eq!(frame.payload(), b"abcde");
    assert_eq!(remainder, second.as_slice());

    // Converting to an owned frame should produce the same payload bytes.
    let owned = frame.into_owned().expect("conversion succeeds");
    assert_eq!(owned.code(), MessageCode::Data);
    assert_eq!(owned.payload(), b"abcde");
}

#[test]
fn borrowed_message_frame_matches_owned_decoding() {
    let encoded = encode_frame(MessageCode::Warning, b"payload");

    let (borrowed, remainder) =
        BorrowedMessageFrame::decode_from_slice(&encoded).expect("decode succeeds");
    assert!(remainder.is_empty());

    let owned = MessageFrame::try_from(encoded.as_slice()).expect("owned decode succeeds");
    assert_eq!(borrowed.code(), owned.code());
    assert_eq!(borrowed.payload(), owned.payload());

    let borrowed_exact =
        BorrowedMessageFrame::try_from(encoded.as_slice()).expect("borrowed decode succeeds");
    assert_eq!(borrowed_exact.code(), owned.code());
    assert_eq!(borrowed_exact.payload(), owned.payload());
}

#[test]
fn decode_from_slice_errors_for_truncated_header() {
    let err = MessageFrame::decode_from_slice(&[0x01, 0x02]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn decode_from_slice_errors_for_truncated_payload() {
    let header = MessageHeader::new(MessageCode::Data, 4).expect("constructible header");
    let mut bytes = Vec::from(header.encode());
    bytes.extend_from_slice(&[0xAA, 0xBB]);

    let err = MessageFrame::decode_from_slice(&bytes).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn message_frame_try_from_slice_round_trips_single_frame() {
    let encoded = encode_frame(MessageCode::Warning, b"payload");
    let frame = MessageFrame::try_from(encoded.as_slice()).expect("decode succeeds");

    assert_eq!(frame.code(), MessageCode::Warning);
    assert_eq!(frame.payload(), b"payload");
}

#[test]
fn message_frame_try_from_slice_rejects_trailing_bytes() {
    let frame = encode_frame(MessageCode::Stats, &[0x01, 0x02, 0x03, 0x04]);
    let mut bytes = frame.clone();
    bytes.extend_from_slice(&[0xFF, 0xEE]);

    let err = MessageFrame::try_from(bytes.as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        "input slice contains 2 trailing bytes after multiplexed frame"
    );
}

#[test]
fn borrowed_message_frame_try_from_rejects_trailing_bytes() {
    let frame = encode_frame(MessageCode::Info, b"hello");
    let mut bytes = frame.clone();
    bytes.push(0xAA);

    let err = BorrowedMessageFrame::try_from(bytes.as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        "input slice contains 1 trailing byte after multiplexed frame"
    );
}

#[test]
fn borrowed_message_frames_iterates_over_sequence() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&encode_frame(MessageCode::Info, b"abc"));
    bytes.extend_from_slice(&encode_frame(MessageCode::Warning, b""));

    let mut iter = BorrowedMessageFrames::new(&bytes);

    let first = iter
        .next()
        .expect("first frame present")
        .expect("decode succeeds");
    assert_eq!(first.code(), MessageCode::Info);
    assert_eq!(first.payload(), b"abc");

    let second = iter
        .next()
        .expect("second frame present")
        .expect("decode succeeds");
    assert_eq!(second.code(), MessageCode::Warning);
    assert!(second.payload().is_empty());

    assert!(iter.next().is_none());
    assert!(iter.remainder().is_empty());
}

#[test]
fn borrowed_message_frames_reports_decode_error() {
    let mut bytes = encode_frame(MessageCode::Info, b"abc");
    let mut truncated = encode_frame(MessageCode::Error, b"payload");
    truncated.pop();
    bytes.extend_from_slice(&truncated);

    let mut iter = BorrowedMessageFrames::new(&bytes);

    let first = iter
        .next()
        .expect("first frame present")
        .expect("decode succeeds");
    assert_eq!(first.code(), MessageCode::Info);

    let err = iter
        .next()
        .expect("error surfaced")
        .expect_err("decode should fail due to truncation");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(iter.next().is_none());
    assert_eq!(iter.remainder(), truncated.as_slice());
}

#[test]
fn borrowed_message_frames_reports_trailing_bytes() {
    let mut bytes = encode_frame(MessageCode::Info, b"abc");
    bytes.push(0xAA);

    let mut iter = BorrowedMessageFrames::new(&bytes);

    let frame = iter
        .next()
        .expect("frame present")
        .expect("decode succeeds");
    assert_eq!(frame.code(), MessageCode::Info);

    let err = iter
        .next()
        .expect("error returned")
        .expect_err("trailing byte should be reported");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(iter.next().is_none());
    assert_eq!(iter.remainder(), &[0xAA][..]);
}

#[test]
fn reserve_payload_extends_capacity_for_empty_buffers() {
    let mut buffer = Vec::with_capacity(4);
    assert!(buffer.capacity() < 10);

    reserve_payload(&mut buffer, 10).expect("reserve succeeds");

    assert!(
        buffer.capacity() >= 10,
        "capacity {} should be at least required length",
        buffer.capacity()
    );
    assert_eq!(buffer.len(), 0, "reserve must not mutate length");
}

#[test]
fn reserve_payload_extends_relative_to_current_length() {
    let mut buffer = Vec::with_capacity(8);
    buffer.extend_from_slice(&[0u8; 3]);
    assert_eq!(buffer.len(), 3);
    assert!(buffer.capacity() < 12);

    reserve_payload(&mut buffer, 12).expect("reserve succeeds");

    assert!(
        buffer.capacity() >= 12,
        "capacity {} should be at least required length",
        buffer.capacity()
    );
    assert_eq!(buffer.len(), 3, "reserve must not mutate length");
}

#[test]
fn send_msg_retries_on_interrupted_vectored_writes() {
    struct InterruptOnceWriter {
        writes: Vec<u8>,
        vectored_attempts: usize,
        vectored_successes: usize,
        interrupted: bool,
    }

    impl InterruptOnceWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                vectored_attempts: 0,
                vectored_successes: 0,
                interrupted: false,
            }
        }
    }

    impl Write for InterruptOnceWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_attempts += 1;
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "simulated EINTR",
                ));
            }

            let mut written = 0;
            for buf in bufs {
                self.writes.extend_from_slice(buf);
                written += buf.len();
            }
            self.vectored_successes += 1;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = InterruptOnceWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Info, payload).expect("retry succeeds after EINTR");

    assert!(writer.interrupted, "writer should have seen an interrupt");
    assert_eq!(writer.vectored_attempts, 2, "exactly one retry expected");
    assert_eq!(
        writer.vectored_successes, 1,
        "second attempt should succeed"
    );

    let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_frame_matches_send_msg_encoding() {
    let payload = b"payload bytes".to_vec();
    let frame = MessageFrame::new(MessageCode::Info, payload).expect("frame is valid");

    let mut via_send_msg = Vec::new();
    send_msg(&mut via_send_msg, frame.code(), frame.payload()).expect("send_msg succeeds");

    let mut via_send_frame = Vec::new();
    send_frame(&mut via_send_frame, &frame).expect("send_frame succeeds");

    assert_eq!(via_send_frame, via_send_msg);
}

#[test]
fn send_frame_handles_empty_payloads() {
    let frame = MessageFrame::new(MessageCode::Error, Vec::new()).expect("frame is valid");

    let mut buffer = Vec::new();
    send_frame(&mut buffer, &frame).expect("send_frame succeeds");

    assert_eq!(buffer.len(), HEADER_LEN);
    assert_eq!(
        &buffer[..HEADER_LEN],
        &MessageHeader::new(frame.code(), 0).unwrap().encode()
    );
}

#[test]
fn message_frame_header_reflects_current_payload_length() {
    let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec()).expect("frame");
    let header = frame.header().expect("header recomputation succeeds");

    assert_eq!(header.code(), MessageCode::Info);
    assert_eq!(header.payload_len(), 3);
}

#[test]
fn message_frame_header_detects_payload_growth_past_limit() {
    let mut frame = MessageFrame::new(MessageCode::Data, Vec::new()).expect("frame");
    frame.payload = vec![0u8; MAX_PAYLOAD_LENGTH as usize + 1];

    let err = frame.header().expect_err("oversized payload must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("multiplexed payload length"),
        "error message should mention the payload limit"
    );
}

#[test]
fn encode_into_writer_matches_send_frame() {
    let frame = MessageFrame::new(MessageCode::Info, b"payload".to_vec()).expect("frame is valid");

    let mut via_method = Vec::new();
    frame
        .encode_into_writer(&mut via_method)
        .expect("method succeeds");

    let mut via_function = Vec::new();
    send_frame(&mut via_function, &frame).expect("send_frame succeeds");

    assert_eq!(via_method, via_function);
}

#[test]
fn encode_into_writer_handles_empty_payloads() {
    let frame = MessageFrame::new(MessageCode::Error, Vec::new()).expect("frame is valid");

    let mut buffer = Vec::new();
    frame
        .encode_into_writer(&mut buffer)
        .expect("method succeeds");

    assert_eq!(buffer.len(), HEADER_LEN);
    assert_eq!(
        &buffer[..HEADER_LEN],
        &MessageHeader::new(frame.code(), 0).unwrap().encode()
    );
}

#[test]
fn encode_into_vec_appends_multiplexed_bytes() {
    let payload = b"encoded payload".to_vec();
    let frame = MessageFrame::new(MessageCode::Info, payload).expect("frame is valid");

    let mut via_encode = vec![0xAA];
    frame
        .encode_into_vec(&mut via_encode)
        .expect("encode_into_vec succeeds");

    let mut via_send = vec![0xAA];
    send_frame(&mut via_send, &frame).expect("send_frame succeeds");

    assert_eq!(via_encode, via_send);
}

#[test]
fn send_msg_vectored_handles_partial_writes() {
    struct ChunkedWriter {
        max_chunk: usize,
        data: Vec<u8>,
        calls: usize,
    }

    impl ChunkedWriter {
        fn new(max_chunk: usize) -> Self {
            Self {
                max_chunk,
                data: Vec::new(),
                calls: 0,
            }
        }
    }

    impl Write for ChunkedWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            panic!("send_msg should rely on vectored writes when available");
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.calls += 1;
            let mut remaining = self.max_chunk;
            let mut written = 0;

            for buf in bufs {
                if buf.is_empty() || remaining == 0 {
                    continue;
                }

                let take = remaining.min(buf.len());
                self.data.extend_from_slice(&buf[..take]);
                remaining -= take;
                written += take;

                if take < buf.len() {
                    break;
                }
            }

            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = ChunkedWriter::new(2);
    let payload = b"abcdefgh";
    send_msg(&mut writer, MessageCode::Client, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Client, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);
    assert_eq!(writer.data, expected);
    assert!(
        writer.calls > 1,
        "partial writes should require multiple calls"
    );
}

#[test]
fn send_msg_vectored_detects_write_zero() {
    struct ZeroWriter;

    impl Write for ZeroWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            panic!("unexpected write fallback");
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = ZeroWriter;
    let err = send_msg(&mut writer, MessageCode::Info, b"payload").unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    assert_eq!(err.to_string(), "failed to write multiplexed message");
}

#[test]
fn send_msg_errors_when_vectored_writer_overreports_progress() {
    struct OverreportingWriter;

    impl Write for OverreportingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            panic!("vectored path should be used");
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            let provided: usize = bufs.iter().map(|buf| buf.len()).sum();
            Ok(provided + 1)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = OverreportingWriter;
    let err = send_msg(&mut writer, MessageCode::Info, b"payload").unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let rendered = err.to_string();
    assert!(
        rendered.contains("writer reported writing 12 bytes"),
        "error message should report written byte count: {rendered}"
    );
    assert!(
        rendered.contains("only 11 bytes were provided"),
        "error message should report available byte count: {rendered}"
    );
}

#[test]
fn send_msg_errors_when_write_overreports_progress_after_fallback() {
    struct OverreportingSequentialWriter {
        vectored_attempts: usize,
    }

    impl OverreportingSequentialWriter {
        fn new() -> Self {
            Self {
                vectored_attempts: 0,
            }
        }
    }

    impl Write for OverreportingSequentialWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len() + 1)
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_attempts += 1;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored IO disabled",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = OverreportingSequentialWriter::new();
    let err = send_msg(&mut writer, MessageCode::Info, b"payload").unwrap_err();

    assert_eq!(
        writer.vectored_attempts, 1,
        "fallback should be attempted once"
    );
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let rendered = err.to_string();
    assert!(
        rendered.contains("writer reported writing 5 bytes"),
        "error message should report overreported progress: {rendered}"
    );
    assert!(
        rendered.contains("only 4 bytes were provided"),
        "error message should mention header length: {rendered}"
    );
}

#[test]
fn round_trip_zero_length_payload() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Warning, b"").expect("send succeeds");

    let mut cursor = io::Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Warning);
    assert!(frame.payload().is_empty());
    assert_eq!(frame.payload_len(), 0);
}

#[test]
fn recv_msg_reports_truncated_payload() {
    let header = MessageHeader::new(MessageCode::Warning, 4)
        .expect("header")
        .encode();
    let mut buffer = header.to_vec();
    buffer.extend_from_slice(&[1, 2]);

    let err = recv_msg(&mut io::Cursor::new(buffer)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        err.to_string(),
        "multiplexed payload truncated: expected 4 bytes but received 2"
    );
}

#[test]
fn recv_msg_into_truncates_buffer_after_short_payload() {
    let header = MessageHeader::new(MessageCode::Client, 4)
        .expect("header")
        .encode();
    let mut data = header.to_vec();
    data.extend_from_slice(&[1, 2]);

    let mut cursor = io::Cursor::new(data);
    let mut buffer = vec![0xAA, 0xBB, 0xCC];
    let err = recv_msg_into(&mut cursor, &mut buffer).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        err.to_string(),
        "multiplexed payload truncated: expected 4 bytes but received 2"
    );
    assert_eq!(buffer, vec![1, 2]);
}

#[test]
fn recv_msg_reports_truncated_header() {
    let mut cursor = io::Cursor::new([0u8; HEADER_LEN - 1]);
    let err = recv_msg(&mut cursor).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn recv_msg_rejects_unknown_message_codes() {
    let unknown_code = 11u8;
    let tag = u32::from(MPLEX_BASE) + u32::from(unknown_code); // MPLEX_BASE + unknown code
    let raw = (tag << 24).to_le_bytes();
    let err = recv_msg(&mut io::Cursor::new(raw)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn recv_msg_rejects_headers_without_mplex_base() {
    let tag_without_base = u32::from(MPLEX_BASE - 1) << 24;
    let err = recv_msg(&mut io::Cursor::new(tag_without_base.to_le_bytes())).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
        err.to_string()
            .contains("multiplexed header contained invalid tag byte")
    );
}

#[test]
fn recv_msg_into_populates_existing_buffer() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Client, b"payload").expect("send succeeds");

    let mut cursor = io::Cursor::new(stream);
    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Client);
    assert_eq!(buffer.as_slice(), b"payload");
}

#[test]
fn message_frame_into_parts_returns_code_and_payload_without_clone() {
    let frame = MessageFrame::new(MessageCode::Warning, b"payload".to_vec()).expect("frame");
    let (code, payload) = frame.into_parts();

    assert_eq!(code, MessageCode::Warning);
    assert_eq!(payload, b"payload");
}

#[test]
fn message_frame_into_payload_returns_owned_payload() {
    let payload = b"payload".to_vec();
    let frame = MessageFrame::new(MessageCode::Warning, payload.clone()).expect("frame");

    let owned = frame.into_payload();

    assert_eq!(owned, payload);
}

#[test]
fn recv_msg_into_reuses_buffer_capacity_for_smaller_payloads() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Warning, b"hi").expect("send succeeds");

    let mut cursor = io::Cursor::new(stream);
    let mut buffer = Vec::with_capacity(64);
    buffer.extend_from_slice(&[0u8; 16]);
    let capacity_before = buffer.capacity();

    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Warning);
    assert_eq!(buffer.as_slice(), b"hi");
    assert_eq!(buffer.capacity(), capacity_before);
}

#[test]
fn recv_msg_into_handles_back_to_back_frames_without_reallocation() {
    let mut stream = Vec::new();
    let first_payload = b"primary payload";
    let second_payload = b"ok";

    send_msg(&mut stream, MessageCode::Info, first_payload).expect("first send");
    send_msg(&mut stream, MessageCode::Warning, second_payload).expect("second send");

    let mut cursor = io::Cursor::new(stream);
    let mut buffer = Vec::new();

    let first_code = recv_msg_into(&mut cursor, &mut buffer).expect("first receive");
    assert_eq!(first_code, MessageCode::Info);
    assert_eq!(buffer.as_slice(), first_payload);

    let capacity_after_first = buffer.capacity();
    let ptr_after_first = buffer.as_ptr();

    let second_code = recv_msg_into(&mut cursor, &mut buffer).expect("second receive");
    assert_eq!(second_code, MessageCode::Warning);
    assert_eq!(buffer.as_slice(), second_payload);
    assert_eq!(buffer.capacity(), capacity_after_first);
    assert_eq!(buffer.as_ptr(), ptr_after_first);
}

#[test]
fn message_frame_payload_mut_allows_in_place_updates() {
    let mut frame = MessageFrame::new(MessageCode::Data, b"payload".to_vec()).expect("frame");

    {
        let payload = frame.payload_mut();
        payload[..4].copy_from_slice(b"data");
    }

    assert_eq!(frame.payload(), b"dataoad");
    assert_eq!(frame.payload_len(), 7);
}

#[test]
fn message_frame_as_ref_exposes_payload_slice() {
    let frame = MessageFrame::new(MessageCode::Warning, b"slice".to_vec()).expect("frame");
    assert_eq!(AsRef::<[u8]>::as_ref(&frame), b"slice");
    let deref: &[u8] = &frame;
    assert_eq!(deref, b"slice");
}

#[test]
fn message_frame_as_mut_allows_mutating_payload_slice() {
    let mut frame = MessageFrame::new(MessageCode::Warning, b"slice".to_vec()).expect("frame");
    let payload = AsMut::<[u8]>::as_mut(&mut frame);
    payload.copy_from_slice(b"PATCH");
    assert_eq!(frame.payload(), b"PATCH");
    {
        let deref: &mut [u8] = &mut frame;
        deref.copy_from_slice(b"lower");
    }
    assert_eq!(frame.payload(), b"lower");
}

#[test]
fn reserve_payload_rejects_capacity_overflow() {
    let mut buffer = Vec::new();
    let err = reserve_payload(&mut buffer, usize::MAX).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
}

#[test]
fn recv_msg_into_clears_buffer_for_empty_payloads() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Log, b"").expect("send succeeds");

    let mut cursor = io::Cursor::new(stream);
    let mut buffer = Vec::with_capacity(8);
    buffer.extend_from_slice(b"junk");
    let capacity_before = buffer.capacity();
    let ptr_before = buffer.as_ptr();

    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Log);
    assert!(buffer.is_empty());
    assert_eq!(buffer.capacity(), capacity_before);
    assert_eq!(buffer.as_ptr(), ptr_before);
}

#[test]
fn send_msg_rejects_oversized_payload() {
    let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = send_msg(&mut io::sink(), MessageCode::Error, &payload).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        format!(
            "multiplexed payload length {} exceeds maximum {}",
            u128::from(MAX_PAYLOAD_LENGTH) + 1,
            u128::from(MAX_PAYLOAD_LENGTH)
        )
    );
}

#[test]
fn message_frame_new_validates_payload_length() {
    let frame = MessageFrame::new(MessageCode::Stats, b"stats".to_vec()).expect("frame");
    assert_eq!(frame.code(), MessageCode::Stats);
    assert_eq!(frame.payload(), b"stats");
}

#[test]
fn message_frame_new_rejects_oversized_payload() {
    let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = MessageFrame::new(MessageCode::Info, payload).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        format!(
            "multiplexed payload length {} exceeds maximum {}",
            u128::from(MAX_PAYLOAD_LENGTH) + 1,
            u128::from(MAX_PAYLOAD_LENGTH)
        )
    );
}

#[test]
fn message_frame_try_from_tuple_constructs_frame() {
    let payload = b"payload".to_vec();
    let frame = MessageFrame::try_from((MessageCode::Warning, payload.clone()))
        .expect("tuple conversion succeeds");
    assert_eq!(frame.code(), MessageCode::Warning);
    assert_eq!(frame.payload(), payload);
}

#[test]
fn message_frame_try_from_tuple_rejects_oversized_payload() {
    let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = MessageFrame::try_from((MessageCode::Info, payload)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains(&format!(
        "multiplexed payload length {}",
        u128::from(MAX_PAYLOAD_LENGTH) + 1
    )));
}

#[test]
fn message_frame_into_tuple_returns_owned_parts() {
    let frame = MessageFrame::new(MessageCode::Deleted, b"done".to_vec()).expect("frame");
    let (code, payload): (MessageCode, Vec<u8>) = frame.into();
    assert_eq!(code, MessageCode::Deleted);
    assert_eq!(payload, b"done");
}

#[test]
fn recv_msg_into_populates_caller_buffer() {
    let mut serialized = Vec::new();
    send_msg(&mut serialized, MessageCode::Warning, b"payload").expect("send succeeds");

    let mut cursor = io::Cursor::new(serialized);
    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Warning);
    assert_eq!(buffer, b"payload");
}

#[test]
fn recv_msg_into_reuses_existing_capacity() {
    let mut serialized = Vec::new();
    send_msg(&mut serialized, MessageCode::Info, b"hello").expect("send succeeds");

    let mut cursor = io::Cursor::new(serialized);
    let mut buffer = vec![0u8; 8];
    let original_capacity = buffer.capacity();
    let original_ptr = buffer.as_ptr();
    buffer.clear();

    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Info);
    assert_eq!(buffer, b"hello");
    assert_eq!(buffer.capacity(), original_capacity);
    assert_eq!(buffer.as_ptr(), original_ptr);
}

#[test]
fn recv_msg_into_handles_empty_payload_without_reading() {
    let mut serialized = Vec::new();
    send_msg(&mut serialized, MessageCode::Log, b"").expect("send succeeds");

    let mut cursor = io::Cursor::new(serialized);
    let mut buffer = vec![1u8; 4];
    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Log);
    assert!(buffer.is_empty());
}

#[test]
fn send_msg_propagates_write_errors() {
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = FailingWriter;
    let err = send_msg(&mut writer, MessageCode::Info, b"payload").unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert!(err.to_string().contains("injected failure"));
}

#[test]
fn reserve_payload_maps_overflow_to_out_of_memory() {
    let mut buffer = Vec::new();
    let err = reserve_payload(&mut buffer, usize::MAX).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
}

#[test]
fn ensure_payload_length_accepts_maximum_payload() {
    let len = MAX_PAYLOAD_LENGTH as usize;
    let validated = ensure_payload_length(len).expect("maximum allowed");

    assert_eq!(validated, MAX_PAYLOAD_LENGTH);
}

#[test]
fn ensure_payload_length_rejects_values_above_limit() {
    let len = MAX_PAYLOAD_LENGTH as usize + 1;
    let err = ensure_payload_length(len).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        format!(
            "multiplexed payload length {} exceeds maximum {}",
            u128::from(MAX_PAYLOAD_LENGTH) + 1,
            u128::from(MAX_PAYLOAD_LENGTH)
        )
    );
}
