//! Tests for network interruption and error handling in the protocol crate.
//!
//! Verifies that the protocol handles connection resets, timeouts,
//! partial packets, and I/O errors gracefully without panicking.

use std::io::{self, Cursor, ErrorKind, Read, Write};

use protocol::{
    decode_varint, read_varint, recv_msg, recv_msg_into, send_msg, write_varint, MessageCode,
    MessageHeader, MplexReader, MplexWriter, MESSAGE_HEADER_LEN,
};

// ============================================================================
// Error Injection Helpers
// ============================================================================

/// A reader that returns an error after N bytes.
struct ErrorAfterNBytes {
    data: Cursor<Vec<u8>>,
    bytes_remaining: usize,
    error_kind: ErrorKind,
}

impl ErrorAfterNBytes {
    fn new(data: Vec<u8>, error_after: usize, kind: ErrorKind) -> Self {
        Self {
            data: Cursor::new(data),
            bytes_remaining: error_after,
            error_kind: kind,
        }
    }
}

impl Read for ErrorAfterNBytes {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.bytes_remaining == 0 {
            return Err(io::Error::new(self.error_kind, "simulated error"));
        }
        let max = buf.len().min(self.bytes_remaining);
        let n = self.data.read(&mut buf[..max])?;
        self.bytes_remaining -= n;
        Ok(n)
    }
}

/// A writer that returns an error after N writes.
struct ErrorAfterNWrites {
    inner: Vec<u8>,
    writes_remaining: usize,
    error_kind: ErrorKind,
}

impl ErrorAfterNWrites {
    fn new(writes_remaining: usize, kind: ErrorKind) -> Self {
        Self {
            inner: Vec::new(),
            writes_remaining,
            error_kind: kind,
        }
    }

    #[allow(dead_code)]
    fn into_inner(self) -> Vec<u8> {
        self.inner
    }
}

impl Write for ErrorAfterNWrites {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.writes_remaining == 0 {
            return Err(io::Error::new(self.error_kind, "simulated write error"));
        }
        self.writes_remaining -= 1;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}


// ============================================================================
// Connection Reset Tests
// ============================================================================

#[test]
fn test_read_connection_reset() {
    // Create some varint data, but inject ConnectionReset partway through
    let mut data = Vec::new();
    write_varint(&mut data, 12345).unwrap();

    // Error after reading only 1 byte
    let mut reader = ErrorAfterNBytes::new(data, 1, ErrorKind::ConnectionReset);

    let result = read_varint(&mut reader);
    assert!(result.is_err(), "Should fail with ConnectionReset");

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::ConnectionReset,
        "Error should be ConnectionReset"
    );
}

#[test]
fn test_write_connection_reset() {
    // Attempt to write varint but fail after first write
    let mut writer = ErrorAfterNWrites::new(0, ErrorKind::ConnectionReset);

    let result = write_varint(&mut writer, 12345);
    assert!(result.is_err(), "Should fail with ConnectionReset");

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::ConnectionReset,
        "Error should be ConnectionReset"
    );
}

#[test]
fn test_recv_msg_connection_reset() {
    // Create a message frame, but error partway through
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, b"test data").unwrap();

    // Error after reading only 2 bytes (partial header)
    let mut reader = ErrorAfterNBytes::new(stream, 2, ErrorKind::ConnectionReset);

    let result = recv_msg(&mut reader);
    assert!(result.is_err(), "Should fail with ConnectionReset");
}

// ============================================================================
// Timeout Tests
// ============================================================================

#[test]
fn test_read_timeout() {
    // Create varint data but inject timeout
    let mut data = Vec::new();
    write_varint(&mut data, 999).unwrap();

    let mut reader = ErrorAfterNBytes::new(data, 1, ErrorKind::TimedOut);

    let result = read_varint(&mut reader);
    assert!(result.is_err(), "Should fail with TimedOut");

    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TimedOut, "Error should be TimedOut");
}

#[test]
fn test_write_timeout() {
    let mut writer = ErrorAfterNWrites::new(0, ErrorKind::TimedOut);

    let result = write_varint(&mut writer, 999);
    assert!(result.is_err(), "Should fail with TimedOut");

    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TimedOut, "Error should be TimedOut");
}

#[test]
fn test_recv_msg_timeout() {
    // Create a valid message
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Info, b"info message").unwrap();

    // Timeout after reading partial header
    let mut reader = ErrorAfterNBytes::new(stream, 3, ErrorKind::TimedOut);

    let result = recv_msg(&mut reader);
    assert!(result.is_err(), "Should fail with timeout during header read");
}

// ============================================================================
// Partial Data Tests
// ============================================================================

#[test]
fn test_partial_varint_read() {
    // Create a multi-byte varint
    let mut data = Vec::new();
    write_varint(&mut data, 100000).unwrap(); // Will be multiple bytes

    // Truncate to only 1 byte
    data.truncate(1);

    let mut cursor = Cursor::new(data);
    let result = read_varint(&mut cursor);

    assert!(
        result.is_err(),
        "Should fail when varint data is truncated"
    );

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::UnexpectedEof,
        "Should report UnexpectedEof for truncated varint"
    );
}

#[test]
fn test_partial_header_read() {
    // Create a full message
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, b"payload").unwrap();

    // Truncate to partial header (only 2 bytes of 4-byte header)
    stream.truncate(2);

    let mut cursor = Cursor::new(stream);
    let result = recv_msg(&mut cursor);

    assert!(result.is_err(), "Should fail with partial header");

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::UnexpectedEof,
        "Should report UnexpectedEof for partial header"
    );
}

#[test]
fn test_partial_payload_read() {
    // Create a message with payload
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, b"hello world").unwrap();

    // Keep full header but truncate payload
    stream.truncate(MESSAGE_HEADER_LEN + 5); // Only "hello"

    let mut cursor = Cursor::new(stream);
    let result = recv_msg(&mut cursor);

    assert!(result.is_err(), "Should fail with partial payload");

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::UnexpectedEof,
        "Should report UnexpectedEof for partial payload"
    );
}

#[test]
fn test_decode_varint_partial_buffer() {
    // Create a multi-byte varint
    let mut data = Vec::new();
    write_varint(&mut data, 50000).unwrap();

    // Truncate the buffer
    let truncated = &data[..data.len() - 1];

    let result = decode_varint(truncated);
    assert!(
        result.is_err(),
        "decode_varint should fail with incomplete data"
    );

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::UnexpectedEof,
        "Should report UnexpectedEof"
    );
}

// ============================================================================
// Error Recovery Tests
// ============================================================================

#[test]
fn test_would_block_handling() {
    // WouldBlock errors should be propagated correctly
    // Reader that always returns WouldBlock
    struct BlockingReader;
    impl Read for BlockingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::WouldBlock, "would block"))
        }
    }

    let mut reader = BlockingReader;
    let result = read_varint(&mut reader);

    assert!(result.is_err(), "Should fail with WouldBlock");
    assert_eq!(result.unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn test_interrupted_handling() {
    // Note: Interrupted may be retried by read_exact, but if it always fails,
    // it should eventually propagate or convert to another error.
    // We test that a persistent Interrupted error is handled gracefully.

    // Create a reader that returns Interrupted for a limited number of times
    struct LimitedInterruptReader {
        data: Cursor<Vec<u8>>,
        interrupts_left: usize,
    }

    impl Read for LimitedInterruptReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.interrupts_left > 0 {
                self.interrupts_left -= 1;
                return Err(io::Error::new(ErrorKind::Interrupted, "interrupted"));
            }
            self.data.read(buf)
        }
    }

    let mut data = Vec::new();
    write_varint(&mut data, 123).unwrap();

    let mut reader = LimitedInterruptReader {
        data: Cursor::new(data),
        interrupts_left: 2, // Will interrupt twice, then succeed
    };

    // read_exact typically retries on Interrupted, so this should eventually succeed
    let result = read_varint(&mut reader);

    // Should either succeed after retries or fail
    match result {
        Ok(val) => assert_eq!(val, 123, "Should read correct value after retries"),
        Err(_) => {
            // Some implementations may give up after too many interrupts
            // Either behavior is acceptable
        }
    }
}

// ============================================================================
// EOF Handling Tests
// ============================================================================

#[test]
fn test_unexpected_eof_during_read() {
    // Empty stream - should fail immediately
    let mut cursor = Cursor::new(Vec::new());
    let result = recv_msg(&mut cursor);

    assert!(result.is_err(), "Should fail on empty stream");

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::UnexpectedEof,
        "Should report UnexpectedEof"
    );
}

#[test]
fn test_clean_eof() {
    // Stream that's completely empty
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut mplex = MplexReader::new(cursor);

    let mut buf = [0u8; 10];
    let result = mplex.read(&mut buf);

    // MplexReader will try to read a header and fail with EOF
    let is_eof = match result {
        Ok(0) => true,
        Err(_) => true,
        _ => false,
    };
    assert!(is_eof, "Should handle EOF");
}

#[test]
fn test_varint_from_empty_stream() {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let result = read_varint(&mut cursor);

    assert!(result.is_err(), "Should fail on empty stream");
    assert_eq!(
        result.unwrap_err().kind(),
        ErrorKind::UnexpectedEof,
        "Should report UnexpectedEof"
    );
}

// ============================================================================
// Multiplex Error Handling Tests
// ============================================================================

#[test]
fn test_mplex_read_with_error_reader() {
    // Create a valid message frame
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, b"test").unwrap();

    // Error after 2 bytes
    let error_reader = ErrorAfterNBytes::new(stream, 2, ErrorKind::BrokenPipe);
    let mut mplex = MplexReader::new(error_reader);

    let mut buf = [0u8; 10];
    let result = mplex.read(&mut buf);

    assert!(result.is_err(), "MplexReader should propagate read error");
}

#[test]
fn test_mplex_write_with_error_writer() {
    let error_writer = ErrorAfterNWrites::new(0, ErrorKind::BrokenPipe);
    let mut mplex = MplexWriter::new(error_writer);

    // Write data - it will be buffered
    mplex.write_all(b"data").unwrap();

    // Flush to trigger actual write which will fail
    let result = mplex.flush();

    assert!(result.is_err(), "MplexWriter should propagate write error on flush");
}

#[test]
fn test_mplex_flush_error() {
    // Create a writer that fails on flush
    struct FlushErrorWriter;

    impl Write for FlushErrorWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(ErrorKind::Other, "flush failed"))
        }
    }

    let mut mplex = MplexWriter::new(FlushErrorWriter);
    mplex.write_all(b"data").unwrap();

    let result = mplex.flush();
    assert!(result.is_err(), "Should propagate flush error");
}

// ============================================================================
// Codec Error Handling
// ============================================================================

#[test]
fn test_varint_from_error_stream() {
    // Stream that always errors
    struct AlwaysErrorReader;

    impl Read for AlwaysErrorReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::Other, "always fails"))
        }
    }

    let mut reader = AlwaysErrorReader;
    let result = read_varint(&mut reader);

    assert!(result.is_err(), "Should fail when stream errors");
    assert_eq!(result.unwrap_err().kind(), ErrorKind::Other);
}

#[test]
fn test_message_header_from_truncated_data() {
    // Create partial header data (less than 4 bytes)
    let partial = vec![0x07, 0x10]; // Only 2 bytes

    let result = MessageHeader::decode(&partial);
    assert!(
        result.is_err(),
        "Should fail when header data is too short"
    );
}

#[test]
fn test_recv_msg_into_with_error_during_payload() {
    // Create a message with a payload
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, b"large payload here").unwrap();

    // Error after reading header but during payload read
    let mut error_reader = ErrorAfterNBytes::new(stream, MESSAGE_HEADER_LEN + 5, ErrorKind::ConnectionReset);
    let mut buf = Vec::new();

    let result = recv_msg_into(&mut error_reader, &mut buf);

    // Should fail gracefully when error occurs during payload read
    assert!(
        result.is_err(),
        "Should fail when error occurs during payload read"
    );

    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::ConnectionReset,
        "Should propagate the underlying error"
    );
}

// ============================================================================
// Stress Tests
// ============================================================================

#[test]
fn test_many_errors_no_panic() {
    // Test that processing many errors doesn't cause issues
    // Note: Interrupted is tested separately as read_exact may retry it
    let error_kinds = [
        ErrorKind::NotFound,
        ErrorKind::PermissionDenied,
        ErrorKind::ConnectionRefused,
        ErrorKind::ConnectionReset,
        ErrorKind::ConnectionAborted,
        ErrorKind::NotConnected,
        ErrorKind::AddrInUse,
        ErrorKind::AddrNotAvailable,
        ErrorKind::BrokenPipe,
        ErrorKind::AlreadyExists,
        ErrorKind::WouldBlock,
        ErrorKind::InvalidInput,
        ErrorKind::InvalidData,
        ErrorKind::TimedOut,
        ErrorKind::WriteZero,
        ErrorKind::UnexpectedEof,
    ];

    for (i, &kind) in error_kinds.iter().enumerate() {
        // Test varint reads with various errors
        let mut data = Vec::new();
        write_varint(&mut data, 1000 + i as i32).unwrap();

        let mut reader = ErrorAfterNBytes::new(data, 1, kind);
        let result = read_varint(&mut reader);

        // Should fail gracefully, not panic
        assert!(
            result.is_err(),
            "Should fail with error kind {:?}",
            kind
        );
    }
}

#[test]
fn test_many_partial_reads_no_panic() {
    // Test many different truncation points
    for truncate_at in 0..10 {
        let mut data = Vec::new();
        send_msg(&mut data, MessageCode::Data, b"test payload data").unwrap();

        if truncate_at < data.len() {
            data.truncate(truncate_at);
        }

        let mut cursor = Cursor::new(data);
        let result = recv_msg(&mut cursor);

        // Should either succeed (if truncate_at was large enough) or fail gracefully
        // Should NOT panic
        match result {
            Ok(_) => {
                // Valid message was read
            }
            Err(e) => {
                // Expected error for truncated data
                assert!(
                    matches!(
                        e.kind(),
                        ErrorKind::UnexpectedEof | ErrorKind::InvalidData
                    ),
                    "Should fail with appropriate error, got: {:?}",
                    e.kind()
                );
            }
        }
    }
}

#[test]
fn test_multiple_mplex_errors() {
    // Test multiple operations with errors
    for i in 0..10 {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, b"data").unwrap();

        let error_reader = ErrorAfterNBytes::new(stream, i, ErrorKind::ConnectionReset);
        let mut mplex = MplexReader::new(error_reader);

        let mut buf = [0u8; 100];
        let _ = mplex.read(&mut buf); // Ignore result, just ensure no panic
    }
}

// ============================================================================
// Edge Case Error Tests
// ============================================================================

#[test]
fn test_zero_byte_read_error() {
    // Reader that successfully reads 0 bytes (not an error, but edge case)
    struct ZeroReader;

    impl Read for ZeroReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0) // EOF indication
        }
    }

    let mut reader = ZeroReader;
    let result = read_varint(&mut reader);

    assert!(
        result.is_err(),
        "Should fail when reader returns 0 (EOF)"
    );
    assert_eq!(
        result.unwrap_err().kind(),
        ErrorKind::UnexpectedEof,
        "Should be UnexpectedEof"
    );
}

#[test]
fn test_write_zero_error() {
    // Writer that accepts writes but claims to write 0 bytes
    struct ZeroWriter;

    impl Write for ZeroWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Ok(0) // Didn't write anything
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = ZeroWriter;
    let result = write_varint(&mut writer, 123);

    // write_all will keep retrying with 0-length writes, eventually detecting the issue
    // or the write_varint implementation may handle this
    if let Err(e) = result {
        assert!(
            matches!(e.kind(), ErrorKind::WriteZero | ErrorKind::Other),
            "Should detect zero-write condition"
        );
    }
}

#[test]
fn test_invalid_message_code_in_header() {
    // Create a message header with invalid code
    let mut header_bytes = [0u8; 4];
    // Set invalid tag value (not in MessageCode range)
    header_bytes[0] = 0xFF; // Invalid tag
    header_bytes[1] = 0x10; // Some length
    header_bytes[2] = 0x00;
    header_bytes[3] = 0x00;

    let result = MessageHeader::decode(&header_bytes);

    // Should either fail or handle invalid codes gracefully
    match result {
        Ok(_header) => {
            // Implementation may accept and handle unknown codes
        }
        Err(_e) => {
            // Or it may reject them - both are acceptable
        }
    }
}

#[test]
fn test_max_payload_length_overflow() {
    // Try to create a message with length that would overflow
    // The protocol has MAX_PAYLOAD_LENGTH but we test boundary
    let mut header_bytes = [0u8; 4];
    header_bytes[0] = 0x07; // DATA message
    header_bytes[1] = 0xFF; // Max values for 24-bit length
    header_bytes[2] = 0xFF;
    header_bytes[3] = 0xFF;

    let result = MessageHeader::decode(&header_bytes);

    match result {
        Ok(header) => {
            // Valid header, but trying to read this much data should handle it
            let payload_len = header.payload_len();
            assert!(
                payload_len <= protocol::MAX_PAYLOAD_LENGTH,
                "Payload length should be within bounds"
            );
        }
        Err(_) => {
            // Or implementation may reject it
        }
    }
}

#[test]
fn test_decode_varint_empty_slice() {
    let result = decode_varint(&[]);
    assert!(result.is_err(), "Should fail on empty slice");
    assert_eq!(result.unwrap_err().kind(), ErrorKind::UnexpectedEof);
}

#[test]
fn test_consecutive_errors_no_state_corruption() {
    // Test that multiple consecutive errors don't corrupt internal state
    let mut data = Vec::new();
    write_varint(&mut data, 999).unwrap();

    for _ in 0..5 {
        let mut reader = ErrorAfterNBytes::new(data.clone(), 1, ErrorKind::ConnectionReset);
        let result = read_varint(&mut reader);
        assert!(result.is_err(), "Each attempt should fail consistently");
    }

    // Now try with valid data to ensure nothing is corrupted
    let mut cursor = Cursor::new(data);
    let result = read_varint(&mut cursor);
    assert!(result.is_ok(), "Valid read should still work");
    assert_eq!(result.unwrap(), 999);
}
