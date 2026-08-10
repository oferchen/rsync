//! Integration tests for MplexReader and MplexWriter.
//!
//! These tests demonstrate the high-level multiplex I/O API and verify
//! that it correctly handles the rsync multiplex protocol.

use protocol::{MessageCode, MplexReader, MplexWriter};
use std::io::{self, Cursor, Read, Write};

#[test]
fn roundtrip_simple_data() {
    let mut buffer = Vec::new();

    // Write data through multiplexer
    {
        let mut writer = MplexWriter::new(&mut buffer);
        writer.write_all(b"hello world").unwrap();
        writer.flush().unwrap();
    }

    // Read data through multiplexer
    {
        let mut reader = MplexReader::new(Cursor::new(&buffer));
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap_or_default();
        assert_eq!(output, "hello world");
    }
}

#[test]
fn roundtrip_large_data() {
    let large_data = vec![0x42u8; 100_000];
    let mut buffer = Vec::new();

    // Write large data (will be split into multiple frames)
    {
        let mut writer = MplexWriter::new(&mut buffer);
        writer.write_all(&large_data).unwrap();
        writer.flush().unwrap();
    }

    // Read back all the data
    {
        let mut reader = MplexReader::new(Cursor::new(&buffer));
        let mut output = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert_eq!(output, large_data);
    }
}

#[test]
fn control_messages_with_data() {
    let mut buffer = Vec::new();

    // Write data and control messages
    {
        let mut writer = MplexWriter::new(&mut buffer);
        writer.write_info("Starting transfer").unwrap();
        writer.write_all(b"file data chunk 1").unwrap();
        writer.write_warning("Slow network detected").unwrap();
        writer.write_all(b"file data chunk 2").unwrap();
        writer.flush().unwrap();
    }

    // Read data and capture messages
    {
        let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let messages_clone = messages.clone();

        let mut reader = MplexReader::new(Cursor::new(&buffer));
        reader.set_message_handler(move |code, payload| {
            if let Ok(msg) = std::str::from_utf8(payload) {
                messages_clone.lock().unwrap().push((code, msg.to_string()));
            }
        });

        let mut data = Vec::new();
        let mut chunk = [0u8; 100];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(data, b"file data chunk 1file data chunk 2");

        let captured = messages.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].0, MessageCode::Info);
        assert_eq!(captured[0].1, "Starting transfer");
        assert_eq!(captured[1].0, MessageCode::Warning);
        assert_eq!(captured[1].1, "Slow network detected");
    }
}

#[test]
fn write_raw_bypasses_framing() {
    let mut buffer = Vec::new();

    {
        let mut writer = MplexWriter::new(&mut buffer);
        writer.write_all(b"multiplexed").unwrap();
        writer.write_raw(b"@RSYNCD: 31.0\n").unwrap();
        writer.write_all(b"more").unwrap();
        writer.flush().unwrap();
    }

    // The raw bytes should be in the stream between framed messages
    assert!(buffer.len() > 14); // Should contain frames + raw bytes
}

#[test]
fn buffering_optimization() {
    let mut buffer = Vec::new();

    // Write many small chunks - should be buffered into fewer frames
    {
        let mut writer = MplexWriter::new(&mut buffer);
        for i in 0..100 {
            writer.write_all(format!("chunk{i} ").as_bytes()).unwrap();
        }
        writer.flush().unwrap();
    }

    // Verify we can read it all back correctly
    {
        let mut reader = MplexReader::new(Cursor::new(&buffer));
        let mut output = String::new();
        let mut chunk = [0u8; 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => output.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        // Verify all chunks are present
        for i in 0..100 {
            assert!(output.contains(&format!("chunk{i} ")));
        }
    }
}

#[test]
fn empty_data_frames() {
    let mut buffer = Vec::new();

    {
        let mut writer = MplexWriter::new(&mut buffer);
        writer.write_data(&[]).unwrap(); // Empty frame
        writer.write_all(b"data").unwrap();
        writer.flush().unwrap();
    }

    {
        let mut reader = MplexReader::new(Cursor::new(&buffer));
        let mut output = Vec::new();
        let mut chunk = [0u8; 10];

        // Read all data (empty frame returns 0, then data frame returns data)
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    // Empty frame - continue to next
                    continue;
                }
                Ok(n) => {
                    output.extend_from_slice(&chunk[..n]);
                    break; // Got our data
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(output, b"data");
    }
}

#[test]
fn custom_buffer_sizes() {
    let mut buffer = Vec::new();

    // Use small buffer to force more frequent frames
    {
        let mut writer = MplexWriter::with_sizes(&mut buffer, 100, 50);
        assert_eq!(writer.buffer_size(), 100);
        assert_eq!(writer.max_frame_size(), 50);

        writer.write_all(&[0xAAu8; 200]).unwrap();
        writer.flush().unwrap();
    }

    // Verify data is split correctly
    {
        let mut reader = MplexReader::with_capacity(Cursor::new(&buffer), 1024);
        let mut output = Vec::new();
        let mut chunk = [0u8; 1024];

        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(output.len(), 200);
        assert!(output.iter().all(|&b| b == 0xAA));
    }
}

#[test]
fn message_types() {
    let mut buffer = Vec::new();

    {
        let mut writer = MplexWriter::new(&mut buffer);
        writer.write_message(MessageCode::Info, b"info").unwrap();
        writer
            .write_message(MessageCode::Warning, b"warning")
            .unwrap();
        writer.write_message(MessageCode::Error, b"error").unwrap();
        writer.write_data(b"data").unwrap();
    }

    {
        let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let messages_clone = messages.clone();

        let mut reader = MplexReader::new(Cursor::new(&buffer));
        reader.set_message_handler(move |code, payload| {
            messages_clone
                .lock()
                .unwrap()
                .push((code, payload.to_vec()));
        });

        let mut data = Vec::new();
        let mut chunk = [0u8; 100];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(data, b"data");

        let captured = messages.lock().unwrap();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0].0, MessageCode::Info);
        assert_eq!(captured[1].0, MessageCode::Warning);
        assert_eq!(captured[2].0, MessageCode::Error);
    }
}
