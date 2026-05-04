use super::*;
use std::io::{self, IoSlice, Write};

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
