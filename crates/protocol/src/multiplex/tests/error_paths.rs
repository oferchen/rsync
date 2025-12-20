use super::*;
use std::io::{self, Cursor, Read, Write};

#[test]
fn recv_msg_propagates_read_errors_during_header() {
    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "access denied",
            ))
        }
    }

    let mut reader = FailingReader;
    let err = recv_msg(&mut reader).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("access denied"));
}

#[test]
fn recv_msg_into_propagates_read_errors_during_header() {
    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "connection lost",
            ))
        }
    }

    let mut reader = FailingReader;
    let mut buffer = Vec::new();
    let err = recv_msg_into(&mut reader, &mut buffer).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
    assert!(err.to_string().contains("connection lost"));
}

#[test]
fn recv_msg_propagates_read_errors_during_payload() {
    struct FailAfterHeaderReader {
        header_read: bool,
    }

    impl FailAfterHeaderReader {
        fn new() -> Self {
            Self { header_read: false }
        }
    }

    impl Read for FailAfterHeaderReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.header_read {
                let header = MessageHeader::new(MessageCode::Data, 10).unwrap().encode();
                let len = buf.len().min(HEADER_LEN);
                buf[..len].copy_from_slice(&header[..len]);
                self.header_read = true;
                Ok(len)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timeout reading payload",
                ))
            }
        }
    }

    let mut reader = FailAfterHeaderReader::new();
    let err = recv_msg(&mut reader).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn recv_msg_into_propagates_read_errors_during_payload() {
    struct FailAfterHeaderReader {
        header_read: bool,
    }

    impl FailAfterHeaderReader {
        fn new() -> Self {
            Self { header_read: false }
        }
    }

    impl Read for FailAfterHeaderReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.header_read {
                let header = MessageHeader::new(MessageCode::Stats, 20).unwrap().encode();
                let len = buf.len().min(HEADER_LEN);
                buf[..len].copy_from_slice(&header[..len]);
                self.header_read = true;
                Ok(len)
            } else {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
            }
        }
    }

    let mut reader = FailAfterHeaderReader::new();
    let mut buffer = Vec::new();
    let err = recv_msg_into(&mut reader, &mut buffer).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn recv_msg_reports_header_truncation_at_each_byte() {
    for truncate_len in 0..HEADER_LEN {
        let header = MessageHeader::new(MessageCode::Info, 0).unwrap().encode();
        let truncated = &header[..truncate_len];

        let err = recv_msg(&mut Cursor::new(truncated)).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::UnexpectedEof,
            "truncation at byte {} should be detected",
            truncate_len
        );
    }
}

#[test]
fn recv_msg_into_reports_header_truncation_at_each_byte() {
    for truncate_len in 0..HEADER_LEN {
        let header = MessageHeader::new(MessageCode::Warning, 5)
            .unwrap()
            .encode();
        let truncated = &header[..truncate_len];

        let mut buffer = Vec::new();
        let err = recv_msg_into(&mut Cursor::new(truncated), &mut buffer).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::UnexpectedEof,
            "truncation at byte {} should be detected",
            truncate_len
        );
    }
}

#[test]
fn recv_msg_reports_payload_truncation_with_details() {
    let header = MessageHeader::new(MessageCode::Data, 100).unwrap().encode();
    let mut data = header.to_vec();
    data.extend_from_slice(&[0xFFu8; 50]);

    let err = recv_msg(&mut Cursor::new(data)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    let message = err.to_string();
    assert!(
        message.contains("expected 100 bytes"),
        "error should mention expected length: {}",
        message
    );
    assert!(
        message.contains("received 50"),
        "error should mention actual length: {}",
        message
    );
}

#[test]
fn recv_msg_into_reports_payload_truncation_with_details() {
    let header = MessageHeader::new(MessageCode::Stats, 200)
        .unwrap()
        .encode();
    let mut data = header.to_vec();
    data.extend_from_slice(&[0xAAu8; 75]);

    let mut buffer = Vec::new();
    let err = recv_msg_into(&mut Cursor::new(data), &mut buffer).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    let message = err.to_string();
    assert!(
        message.contains("expected 200 bytes"),
        "error should mention expected length: {}",
        message
    );
    assert!(
        message.contains("received 75"),
        "error should mention actual length: {}",
        message
    );
    assert_eq!(buffer.len(), 75, "partial payload should be in buffer");
}

#[test]
fn recv_msg_rejects_tags_below_mplex_base() {
    for tag in 0..MPLEX_BASE {
        let raw_header = ((tag as u32) << 24).to_le_bytes();
        let err = recv_msg(&mut Cursor::new(raw_header)).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "tag {} below MPLEX_BASE should be rejected",
            tag
        );
        assert!(
            err.to_string().contains("invalid tag byte"),
            "error message should mention invalid tag: {err}",
        );
    }
}

#[test]
fn recv_msg_into_rejects_tags_below_mplex_base() {
    for tag in 0..MPLEX_BASE {
        let raw_header = ((tag as u32) << 24).to_le_bytes();
        let mut buffer = Vec::new();
        let err = recv_msg_into(&mut Cursor::new(raw_header), &mut buffer).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "tag {} below MPLEX_BASE should be rejected",
            tag
        );
    }
}

#[test]
fn recv_msg_rejects_unknown_message_codes_in_valid_range() {
    let invalid_offsets = [
        11u8, 12, 15, 21, 23, 25, 30, 34, 40, 43, 50, 85, 87, 99, 103, 200,
    ];

    for &offset in &invalid_offsets {
        let tag = u32::from(MPLEX_BASE) + u32::from(offset);
        let raw_header = (tag << 24).to_le_bytes();
        let err = recv_msg(&mut Cursor::new(raw_header)).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "unknown code offset {} should be rejected",
            offset
        );
    }
}

#[test]
fn send_msg_validates_payload_length_before_writing() {
    struct TrackingWriter {
        write_count: usize,
    }

    impl Write for TrackingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            self.write_count += 1;
            panic!("write should not be called for oversized payload");
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = TrackingWriter { write_count: 0 };
    let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = send_msg(&mut writer, MessageCode::Data, &oversized).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        writer.write_count, 0,
        "no write should occur for oversized payload"
    );
}

#[test]
fn recv_msg_handles_exact_header_length_stream() {
    let header = MessageHeader::new(MessageCode::NoOp, 0).unwrap().encode();
    let data = header.to_vec();

    let frame = recv_msg(&mut Cursor::new(data)).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::NoOp);
    assert_eq!(frame.payload().len(), 0);
}

#[test]
fn recv_msg_into_handles_exact_header_length_stream() {
    let header = MessageHeader::new(MessageCode::ErrorExit, 0)
        .unwrap()
        .encode();
    let data = header.to_vec();

    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut Cursor::new(data), &mut buffer).expect("receive succeeds");
    assert_eq!(code, MessageCode::ErrorExit);
    assert!(buffer.is_empty());
}

#[test]
fn send_msg_handles_partial_writes_in_fallback_mode() {
    struct PartialFallbackWriter {
        chunks_written: Vec<usize>,
    }

    impl Write for PartialFallbackWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let chunk_size = (buf.len() / 2).max(1);
            self.chunks_written.push(chunk_size);
            Ok(chunk_size)
        }

        fn write_vectored(&mut self, _bufs: &[std::io::IoSlice<'_>]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no vectored"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = PartialFallbackWriter {
        chunks_written: Vec::new(),
    };
    send_msg(&mut writer, MessageCode::Info, b"test payload").expect("send succeeds");

    assert!(
        writer.chunks_written.len() > 1,
        "partial writes should require multiple chunks"
    );
}

#[test]
fn recv_msg_handles_payload_read_in_multiple_chunks() {
    struct ChunkedReader {
        data: Vec<u8>,
        offset: usize,
        chunk_size: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.offset >= self.data.len() {
                return Ok(0);
            }

            let available = self.data.len() - self.offset;
            let to_read = available.min(buf.len()).min(self.chunk_size);
            buf[..to_read].copy_from_slice(&self.data[self.offset..self.offset + to_read]);
            self.offset += to_read;
            Ok(to_read)
        }
    }

    let payload = vec![0xBBu8; 100];
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, &payload).expect("send succeeds");

    let mut reader = ChunkedReader {
        data: stream,
        offset: 0,
        chunk_size: 3,
    };

    let frame = recv_msg(&mut reader).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Data);
    assert_eq!(frame.payload().len(), 100);
    assert!(frame.payload().iter().all(|&b| b == 0xBB));
}

#[test]
fn recv_msg_into_handles_payload_read_in_multiple_chunks() {
    struct ChunkedReader {
        data: Vec<u8>,
        offset: usize,
        chunk_size: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.offset >= self.data.len() {
                return Ok(0);
            }

            let available = self.data.len() - self.offset;
            let to_read = available.min(buf.len()).min(self.chunk_size);
            buf[..to_read].copy_from_slice(&self.data[self.offset..self.offset + to_read]);
            self.offset += to_read;
            Ok(to_read)
        }
    }

    let payload = vec![0xDDu8; 150];
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Stats, &payload).expect("send succeeds");

    let mut reader = ChunkedReader {
        data: stream,
        offset: 0,
        chunk_size: 7,
    };

    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut reader, &mut buffer).expect("receive succeeds");
    assert_eq!(code, MessageCode::Stats);
    assert_eq!(buffer.len(), 150);
    assert!(buffer.iter().all(|&b| b == 0xDD));
}

#[test]
fn send_msg_detects_write_zero_in_fallback_mode() {
    struct ZeroFallbackWriter;

    impl Write for ZeroFallbackWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn write_vectored(&mut self, _bufs: &[std::io::IoSlice<'_>]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no vectored"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = ZeroFallbackWriter;
    let err = send_msg(&mut writer, MessageCode::Warning, b"test").unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    assert_eq!(err.to_string(), "failed to write multiplexed message");
}

#[test]
fn recv_msg_handles_empty_reader() {
    let empty: &[u8] = &[];
    let err = recv_msg(&mut Cursor::new(empty)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn recv_msg_into_handles_empty_reader() {
    let empty: &[u8] = &[];
    let mut buffer = Vec::new();
    let err = recv_msg_into(&mut Cursor::new(empty), &mut buffer).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn send_msg_handles_interrupted_writes_in_fallback_mode() {
    struct InterruptingFallbackWriter {
        interrupts: usize,
        max_interrupts: usize,
        data: Vec<u8>,
    }

    impl Write for InterruptingFallbackWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.interrupts < self.max_interrupts {
                self.interrupts += 1;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "EINTR"));
            }
            self.data.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, _bufs: &[std::io::IoSlice<'_>]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no vectored"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = InterruptingFallbackWriter {
        interrupts: 0,
        max_interrupts: 3,
        data: Vec::new(),
    };

    send_msg(&mut writer, MessageCode::Client, b"payload").expect("send succeeds after retries");

    assert_eq!(writer.interrupts, 3);
    let header = MessageHeader::new(MessageCode::Client, 7).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(b"payload");
    assert_eq!(writer.data, expected);
}

#[test]
fn recv_msg_handles_interrupted_reads_during_header() {
    struct InterruptingReader {
        data: Vec<u8>,
        offset: usize,
        interrupts: usize,
        max_interrupts: usize,
    }

    impl Read for InterruptingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.interrupts < self.max_interrupts {
                self.interrupts += 1;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "EINTR"));
            }

            if self.offset >= self.data.len() {
                return Ok(0);
            }

            let to_read = (self.data.len() - self.offset).min(buf.len());
            buf[..to_read].copy_from_slice(&self.data[self.offset..self.offset + to_read]);
            self.offset += to_read;
            Ok(to_read)
        }
    }

    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Info, b"interrupted").expect("send succeeds");

    let mut reader = InterruptingReader {
        data: stream,
        offset: 0,
        interrupts: 0,
        max_interrupts: 2,
    };

    let frame = recv_msg(&mut reader).expect("receive succeeds after interrupts");
    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"interrupted");
    assert_eq!(reader.interrupts, 2);
}

#[test]
fn recv_msg_into_handles_interrupted_reads_during_payload() {
    struct InterruptingReader {
        data: Vec<u8>,
        offset: usize,
        interrupts: usize,
        max_interrupts: usize,
    }

    impl Read for InterruptingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.offset >= HEADER_LEN && self.interrupts < self.max_interrupts {
                self.interrupts += 1;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "EINTR"));
            }

            if self.offset >= self.data.len() {
                return Ok(0);
            }

            let to_read = (self.data.len() - self.offset).min(buf.len());
            buf[..to_read].copy_from_slice(&self.data[self.offset..self.offset + to_read]);
            self.offset += to_read;
            Ok(to_read)
        }
    }

    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Stats, b"interrupted payload").expect("send succeeds");

    let mut reader = InterruptingReader {
        data: stream,
        offset: 0,
        interrupts: 0,
        max_interrupts: 3,
    };

    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut reader, &mut buffer).expect("receive succeeds after interrupts");
    assert_eq!(code, MessageCode::Stats);
    assert_eq!(buffer, b"interrupted payload");
    assert_eq!(reader.interrupts, 3);
}
