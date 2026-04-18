use std::io::{self, Cursor, IoSlice, Write};

use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

use super::super::frame::MessageFrame;
use super::send::write_all_vectored;
use super::{recv_msg, send_frame, send_msg, send_msgs_vectored};

#[test]
fn send_msg_empty_payload() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, &[]).unwrap();
    assert_eq!(buffer.len(), HEADER_LEN);
}

#[test]
fn send_msg_with_payload() {
    let mut buffer = Vec::new();
    let payload = b"hello world";
    send_msg(&mut buffer, MessageCode::Data, payload).unwrap();
    assert_eq!(buffer.len(), HEADER_LEN + payload.len());
}

#[test]
fn send_msg_preserves_payload_content() {
    let mut buffer = Vec::new();
    let payload = b"test payload data";
    send_msg(&mut buffer, MessageCode::Data, payload).unwrap();
    assert_eq!(&buffer[HEADER_LEN..], payload.as_slice());
}

#[test]
fn send_msg_encodes_message_code() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::ErrorXfer, b"error").unwrap();
    let header = MessageHeader::decode(&buffer[..HEADER_LEN]).unwrap();
    assert_eq!(header.code(), MessageCode::ErrorXfer);
}

#[test]
fn send_msg_encodes_payload_length() {
    let mut buffer = Vec::new();
    let payload = vec![0u8; 1000];
    send_msg(&mut buffer, MessageCode::Data, &payload).unwrap();
    let header = MessageHeader::decode(&buffer[..HEADER_LEN]).unwrap();
    assert_eq!(header.payload_len_usize(), 1000);
}

#[test]
fn recv_msg_empty_payload() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, &[]).unwrap();

    let mut reader = Cursor::new(buffer);
    let frame = recv_msg(&mut reader).unwrap();
    assert_eq!(frame.code(), MessageCode::Data);
    assert!(frame.payload().is_empty());
}

#[test]
fn recv_msg_with_payload() {
    let payload = b"test message content";
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, payload).unwrap();

    let mut reader = Cursor::new(buffer);
    let frame = recv_msg(&mut reader).unwrap();
    assert_eq!(frame.code(), MessageCode::Data);
    assert_eq!(frame.payload(), payload.as_slice());
}

#[test]
fn recv_msg_different_codes() {
    for code in [
        MessageCode::Data,
        MessageCode::ErrorXfer,
        MessageCode::Info,
        MessageCode::Error,
        MessageCode::Warning,
        MessageCode::Log,
    ] {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, code, b"test").unwrap();

        let mut reader = Cursor::new(buffer);
        let frame = recv_msg(&mut reader).unwrap();
        assert_eq!(frame.code(), code);
    }
}

#[test]
fn recv_msg_eof_returns_error() {
    let mut reader = Cursor::new(Vec::new());
    let result = recv_msg(&mut reader);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn recv_msg_truncated_header_returns_error() {
    let mut reader = Cursor::new(vec![0u8, 1u8]);
    let result = recv_msg(&mut reader);
    assert!(result.is_err());
}

#[test]
fn recv_msg_into_reuses_buffer() {
    let payload = b"buffer reuse test";
    let mut send_buffer = Vec::new();
    send_msg(&mut send_buffer, MessageCode::Data, payload).unwrap();

    let mut reader = Cursor::new(send_buffer);
    let mut recv_buffer = Vec::with_capacity(100);
    let code = super::recv_msg_into(&mut reader, &mut recv_buffer).unwrap();

    assert_eq!(code, MessageCode::Data);
    assert_eq!(recv_buffer, payload.as_slice());
}

#[test]
fn recv_msg_into_clears_buffer() {
    let payload = b"new content";
    let mut send_buffer = Vec::new();
    send_msg(&mut send_buffer, MessageCode::Data, payload).unwrap();

    let mut reader = Cursor::new(send_buffer);
    let mut recv_buffer = vec![0xFFu8; 50];
    let _code = super::recv_msg_into(&mut reader, &mut recv_buffer).unwrap();

    assert_eq!(recv_buffer.len(), payload.len());
    assert_eq!(recv_buffer, payload.as_slice());
}

#[test]
fn recv_msg_into_empty_payload() {
    let mut send_buffer = Vec::new();
    send_msg(&mut send_buffer, MessageCode::Info, &[]).unwrap();

    let mut reader = Cursor::new(send_buffer);
    let mut recv_buffer = vec![1, 2, 3];
    let code = super::recv_msg_into(&mut reader, &mut recv_buffer).unwrap();

    assert_eq!(code, MessageCode::Info);
    assert!(recv_buffer.is_empty());
}

#[test]
fn send_frame_matches_send_msg() {
    let payload = b"frame test";

    let mut buffer1 = Vec::new();
    send_msg(&mut buffer1, MessageCode::Data, payload).unwrap();

    let frame = MessageFrame::new(MessageCode::Data, payload.to_vec()).unwrap();
    let mut buffer2 = Vec::new();
    send_frame(&mut buffer2, &frame).unwrap();

    assert_eq!(buffer1, buffer2);
}

#[test]
fn send_frame_empty_payload() {
    let frame = MessageFrame::new(MessageCode::Warning, Vec::new()).unwrap();
    let mut buffer = Vec::new();
    send_frame(&mut buffer, &frame).unwrap();
    assert_eq!(buffer.len(), HEADER_LEN);
}

#[test]
fn roundtrip_preserves_message() {
    let original_payload = b"roundtrip test with special chars: \x00\x01\xff";

    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, original_payload).unwrap();

    let mut reader = Cursor::new(buffer);
    let frame = recv_msg(&mut reader).unwrap();

    assert_eq!(frame.code(), MessageCode::Data);
    assert_eq!(frame.payload(), original_payload.as_slice());
}

#[test]
fn roundtrip_multiple_messages() {
    let messages = [
        (MessageCode::Data, b"first message".as_slice()),
        (MessageCode::Info, b"second message".as_slice()),
        (MessageCode::Warning, b"third message".as_slice()),
    ];

    let mut buffer = Vec::new();
    for (code, payload) in &messages {
        send_msg(&mut buffer, *code, payload).unwrap();
    }

    let mut reader = Cursor::new(buffer);
    for (expected_code, expected_payload) in &messages {
        let frame = recv_msg(&mut reader).unwrap();
        assert_eq!(frame.code(), *expected_code);
        assert_eq!(frame.payload(), *expected_payload);
    }
}

#[test]
fn roundtrip_large_payload() {
    let large_payload = vec![0xABu8; 65535];

    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, &large_payload).unwrap();

    let mut reader = Cursor::new(buffer);
    let frame = recv_msg(&mut reader).unwrap();

    assert_eq!(frame.payload(), large_payload.as_slice());
}

#[test]
fn write_all_vectored_empty_slices() {
    let mut buffer = Vec::new();
    write_all_vectored(&mut buffer, &[], &[]).unwrap();
    assert!(buffer.is_empty());
}

#[test]
fn write_all_vectored_header_only() {
    let mut buffer = Vec::new();
    let header = b"HEAD";
    write_all_vectored(&mut buffer, header, &[]).unwrap();
    assert_eq!(buffer, header.as_slice());
}

#[test]
fn write_all_vectored_payload_only() {
    let mut buffer = Vec::new();
    let payload = b"PAYLOAD";
    write_all_vectored(&mut buffer, &[], payload).unwrap();
    assert_eq!(buffer, payload.as_slice());
}

#[test]
fn write_all_vectored_both_slices() {
    let mut buffer = Vec::new();
    let header = b"HEADER";
    let payload = b"PAYLOAD";
    write_all_vectored(&mut buffer, header, payload).unwrap();
    assert_eq!(buffer, b"HEADERPAYLOAD".as_slice());
}

#[test]
fn send_msgs_vectored_empty() {
    let mut buffer = Vec::new();
    send_msgs_vectored(&mut buffer, &[]).unwrap();
    assert!(buffer.is_empty());
}

#[test]
fn send_msgs_vectored_single_message() {
    let mut buffer = Vec::new();
    let messages = [(MessageCode::Info, b"test".as_slice())];
    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut expected = Vec::new();
    send_msg(&mut expected, MessageCode::Info, b"test").unwrap();
    assert_eq!(buffer, expected);
}

#[test]
fn send_msgs_vectored_multiple_messages() {
    let mut buffer = Vec::new();
    let messages = [
        (MessageCode::Info, b"first".as_slice()),
        (MessageCode::Warning, b"second".as_slice()),
        (MessageCode::Error, b"third".as_slice()),
    ];
    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.code(), MessageCode::Info);
    assert_eq!(frame1.payload(), b"first");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.code(), MessageCode::Warning);
    assert_eq!(frame2.payload(), b"second");

    let frame3 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame3.code(), MessageCode::Error);
    assert_eq!(frame3.payload(), b"third");
}

#[test]
fn send_msgs_vectored_with_empty_payloads() {
    let mut buffer = Vec::new();
    let messages = [
        (MessageCode::Info, b"".as_slice()),
        (MessageCode::Data, b"content".as_slice()),
        (MessageCode::Warning, b"".as_slice()),
    ];
    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.code(), MessageCode::Info);
    assert!(frame1.payload().is_empty());

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.code(), MessageCode::Data);
    assert_eq!(frame2.payload(), b"content");

    let frame3 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame3.code(), MessageCode::Warning);
    assert!(frame3.payload().is_empty());
}

#[test]
fn send_msgs_vectored_matches_sequential_sends() {
    let messages = [
        (MessageCode::Info, b"first message".as_slice()),
        (MessageCode::Data, b"some data content".as_slice()),
        (MessageCode::Warning, b"warning text".as_slice()),
    ];

    let mut vectored_buffer = Vec::new();
    send_msgs_vectored(&mut vectored_buffer, &messages).unwrap();

    let mut sequential_buffer = Vec::new();
    for (code, payload) in &messages {
        send_msg(&mut sequential_buffer, *code, payload).unwrap();
    }

    assert_eq!(vectored_buffer, sequential_buffer);
}

#[test]
fn send_msgs_vectored_large_batch() {
    let mut buffer = Vec::new();

    let payload = b"test payload data";
    let messages: Vec<_> = (0..100)
        .map(|_| (MessageCode::Data, payload.as_slice()))
        .collect();

    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    for _ in 0..100 {
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert_eq!(frame.payload(), payload);
    }
}

#[test]
fn send_msgs_vectored_validates_payload_size() {
    use crate::MAX_PAYLOAD_LENGTH;

    let mut buffer = Vec::new();
    let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let messages = [(MessageCode::Data, oversized.as_slice())];

    let result = send_msgs_vectored(&mut buffer, &messages);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn send_msgs_vectored_mixed_sizes() {
    let mut buffer = Vec::new();
    let small = b"x";
    let medium = vec![0u8; 1000];
    let large = vec![0u8; 10000];

    let messages = [
        (MessageCode::Info, small.as_slice()),
        (MessageCode::Data, medium.as_slice()),
        (MessageCode::Warning, large.as_slice()),
    ];

    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.payload().len(), 1);

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.payload().len(), 1000);

    let frame3 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame3.payload().len(), 10000);
}

#[test]
fn send_msgs_vectored_fallback_to_sequential() {
    struct NonVectoredWriter {
        buffer: Vec<u8>,
    }

    impl Write for NonVectoredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored I/O not supported",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = NonVectoredWriter { buffer: Vec::new() };
    let messages = [
        (MessageCode::Info, b"first".as_slice()),
        (MessageCode::Data, b"second".as_slice()),
    ];

    send_msgs_vectored(&mut writer, &messages).unwrap();

    let mut cursor = Cursor::new(&writer.buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.code(), MessageCode::Info);
    assert_eq!(frame1.payload(), b"first");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.code(), MessageCode::Data);
    assert_eq!(frame2.payload(), b"second");
}

#[test]
fn send_msgs_vectored_handles_partial_writes() {
    struct PartialWriter {
        buffer: Vec<u8>,
        chunk_size: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let to_write = buf.len().min(self.chunk_size);
            self.buffer.extend_from_slice(&buf[..to_write]);
            Ok(to_write)
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            let mut remaining = self.chunk_size;
            for buf in bufs {
                if remaining == 0 {
                    break;
                }
                let to_write = buf.len().min(remaining);
                self.buffer.extend_from_slice(&buf[..to_write]);
                remaining -= to_write;
            }
            Ok(self.chunk_size - remaining)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = PartialWriter {
        buffer: Vec::new(),
        chunk_size: 10,
    };

    let messages = [
        (MessageCode::Info, b"message one".as_slice()),
        (MessageCode::Data, b"message two".as_slice()),
    ];

    send_msgs_vectored(&mut writer, &messages).unwrap();

    let mut cursor = Cursor::new(&writer.buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.payload(), b"message one");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.payload(), b"message two");
}

#[test]
fn send_msgs_vectored_detects_write_zero() {
    struct ZeroWriter;

    impl Write for ZeroWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = ZeroWriter;
    let messages = [(MessageCode::Info, b"test".as_slice())];

    let result = send_msgs_vectored(&mut writer, &messages);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WriteZero);
}

#[test]
fn send_msgs_vectored_all_message_codes() {
    let mut buffer = Vec::new();
    let payload = b"test";

    for code in MessageCode::ALL {
        buffer.clear();
        let messages = [(code, payload.as_slice())];
        send_msgs_vectored(&mut buffer, &messages).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), code);
        assert_eq!(frame.payload(), payload);
    }
}

#[test]
fn send_msgs_vectored_single_empty_message() {
    let mut buffer = Vec::new();
    let messages = [(MessageCode::Info, b"".as_slice())];
    send_msgs_vectored(&mut buffer, &messages).unwrap();

    assert_eq!(buffer.len(), HEADER_LEN);

    let mut cursor = Cursor::new(&buffer);
    let frame = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame.code(), MessageCode::Info);
    assert!(frame.payload().is_empty());
}

#[test]
fn send_msgs_vectored_all_empty_messages() {
    let mut buffer = Vec::new();
    let messages = [
        (MessageCode::Info, b"".as_slice()),
        (MessageCode::Data, b"".as_slice()),
        (MessageCode::Warning, b"".as_slice()),
    ];
    send_msgs_vectored(&mut buffer, &messages).unwrap();

    assert_eq!(buffer.len(), HEADER_LEN * 3);

    let mut cursor = Cursor::new(&buffer);
    for (expected_code, _) in &messages {
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), *expected_code);
        assert!(frame.payload().is_empty());
    }
}

#[test]
fn send_msgs_vectored_very_large_payload() {
    let mut buffer = Vec::new();
    let max_size = crate::MAX_PAYLOAD_LENGTH as usize;
    let large_payload = vec![0x42u8; max_size];
    let messages = [(MessageCode::Data, large_payload.as_slice())];

    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    let frame = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame.code(), MessageCode::Data);
    assert_eq!(frame.payload().len(), max_size);
    assert!(frame.payload().iter().all(|&b| b == 0x42));
}

#[test]
fn send_msgs_vectored_multiple_large_payloads() {
    let mut buffer = Vec::new();
    let large1 = vec![0xAAu8; 50000];
    let large2 = vec![0xBBu8; 60000];
    let large3 = vec![0xCCu8; 70000];

    let messages = [
        (MessageCode::Data, large1.as_slice()),
        (MessageCode::Info, large2.as_slice()),
        (MessageCode::Warning, large3.as_slice()),
    ];

    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.payload().len(), 50000);
    assert!(frame1.payload().iter().all(|&b| b == 0xAA));

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.payload().len(), 60000);
    assert!(frame2.payload().iter().all(|&b| b == 0xBB));

    let frame3 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame3.payload().len(), 70000);
    assert!(frame3.payload().iter().all(|&b| b == 0xCC));
}

#[test]
fn send_msgs_vectored_validates_all_messages_before_writing() {
    use crate::MAX_PAYLOAD_LENGTH;

    let mut buffer = Vec::new();
    let valid_payload = b"valid";
    let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];

    let messages = [
        (MessageCode::Info, valid_payload.as_slice()),
        (MessageCode::Data, oversized.as_slice()),
    ];

    let result = send_msgs_vectored(&mut buffer, &messages);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);

    assert!(buffer.is_empty());
}

#[test]
fn send_msgs_vectored_binary_payloads() {
    let mut buffer = Vec::new();
    let binary1 = vec![0u8, 1, 2, 255, 254, 253];
    let binary2: Vec<u8> = (0..=255).collect();
    let binary3 = vec![0xFFu8; 100];

    let messages = [
        (MessageCode::Data, binary1.as_slice()),
        (MessageCode::Info, binary2.as_slice()),
        (MessageCode::Warning, binary3.as_slice()),
    ];

    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.payload(), binary1);

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.payload(), binary2);

    let frame3 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame3.payload(), binary3);
}

#[test]
fn send_msgs_vectored_handles_interrupted_write() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct InterruptingWriter {
        buffer: Vec<u8>,
        interrupt_count: Arc<AtomicUsize>,
    }

    impl Write for InterruptingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.interrupt_count.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
            }
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            if self.interrupt_count.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
            }
            let mut total = 0;
            for buf in bufs {
                self.buffer.extend_from_slice(buf);
                total += buf.len();
            }
            Ok(total)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let interrupt_count = Arc::new(AtomicUsize::new(3));
    let mut writer = InterruptingWriter {
        buffer: Vec::new(),
        interrupt_count: interrupt_count.clone(),
    };

    let messages = [
        (MessageCode::Info, b"first".as_slice()),
        (MessageCode::Data, b"second".as_slice()),
    ];

    send_msgs_vectored(&mut writer, &messages).unwrap();

    let mut cursor = Cursor::new(&writer.buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.payload(), b"first");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.payload(), b"second");
}

#[test]
fn send_msgs_vectored_very_many_messages() {
    let mut buffer = Vec::new();
    let count = 1000;
    let payload = b"msg";
    let messages: Vec<_> = (0..count)
        .map(|_| (MessageCode::Data, payload.as_slice()))
        .collect();

    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    for _ in 0..count {
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert_eq!(frame.payload(), payload);
    }
}

#[test]
fn send_msgs_vectored_detects_writer_overreporting() {
    struct OverreportingWriter {
        buffer: Vec<u8>,
    }

    impl Write for OverreportingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            Ok(buf.len() + 100)
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            let mut total = 0;
            for buf in bufs {
                self.buffer.extend_from_slice(buf);
                total += buf.len();
            }
            Ok(total + 100)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = OverreportingWriter { buffer: Vec::new() };
    let messages = [(MessageCode::Info, b"test".as_slice())];

    let result = send_msgs_vectored(&mut writer, &messages);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn send_msgs_vectored_propagates_write_errors() {
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = FailingWriter;
    let messages = [(MessageCode::Info, b"test".as_slice())];

    let result = send_msgs_vectored(&mut writer, &messages);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn send_msgs_vectored_single_byte_payloads() {
    let mut buffer = Vec::new();
    let messages = [
        (MessageCode::Info, b"a".as_slice()),
        (MessageCode::Data, b"b".as_slice()),
        (MessageCode::Warning, b"c".as_slice()),
        (MessageCode::Error, b"d".as_slice()),
    ];

    send_msgs_vectored(&mut buffer, &messages).unwrap();

    let mut cursor = Cursor::new(&buffer);
    let expected = [b"a", b"b", b"c", b"d"];
    for payload in &expected {
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.payload(), payload.as_slice());
    }
}

#[test]
fn send_msgs_vectored_partial_write_across_message_boundary() {
    struct BoundaryWriter {
        buffer: Vec<u8>,
        write_count: usize,
    }

    impl Write for BoundaryWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_count += 1;
            let to_write = if self.write_count <= 3 {
                buf.len().min(7)
            } else {
                buf.len()
            };
            self.buffer.extend_from_slice(&buf[..to_write]);
            Ok(to_write)
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.write_count += 1;
            let limit = if self.write_count <= 3 { 7 } else { usize::MAX };
            let mut written = 0;
            for buf in bufs {
                if written >= limit {
                    break;
                }
                let to_write = (limit - written).min(buf.len());
                self.buffer.extend_from_slice(&buf[..to_write]);
                written += to_write;
                if written >= limit {
                    break;
                }
            }
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = BoundaryWriter {
        buffer: Vec::new(),
        write_count: 0,
    };

    let messages = [
        (MessageCode::Info, b"first message".as_slice()),
        (MessageCode::Data, b"second message".as_slice()),
    ];

    send_msgs_vectored(&mut writer, &messages).unwrap();

    let mut cursor = Cursor::new(&writer.buffer);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.payload(), b"first message");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.payload(), b"second message");
}
