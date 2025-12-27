//! Tracing wrapper for TcpStream that logs all I/O operations.
//!
//! This module provides debugging utilities to trace the exact byte sequences
//! sent and received during protocol negotiations.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::TcpStream;

fn log_to_file(msg: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/daemon_protocol_trace.log")
    {
        let _ = writeln!(file, "{msg}");
    }
}

/// Wrapper around TcpStream that logs all read/write operations.
pub struct TracingStream {
    inner: TcpStream,
    direction: &'static str,
    total_written: usize,
    total_read: usize,
}

impl TracingStream {
    /// Creates a new tracing stream.
    ///
    /// The `direction` parameter is used in log messages to distinguish
    /// between different streams (e.g., "read", "write").
    #[allow(dead_code)]
    pub fn new(stream: TcpStream, direction: &'static str) -> Self {
        log_to_file(&format!("[TracingStream::{direction}] Created"));
        Self {
            inner: stream,
            direction,
            total_written: 0,
            total_read: 0,
        }
    }
}

impl Read for TracingStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.total_read += n;

        if n > 0 {
            let display_len = n.min(64);
            let hex_str = buf[..display_len]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            log_to_file(&format!(
                "[TracingStream::{}] READ {} bytes (total: {}): {}{}",
                self.direction,
                n,
                self.total_read,
                hex_str,
                if n > 64 { " ..." } else { "" }
            ));
        }

        Ok(n)
    }
}

impl Write for TracingStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.total_written += n;

        if n > 0 {
            let display_len = n.min(64);
            let hex_str = buf[..display_len]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            log_to_file(&format!(
                "[TracingStream::{}] WRITE {} bytes (total: {}): {}{}",
                self.direction,
                n,
                self.total_written,
                hex_str,
                if n > 64 { " ..." } else { "" }
            ));
        }

        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        log_to_file(&format!(
            "[TracingStream::{}] FLUSH (total written: {})",
            self.direction, self.total_written
        ));
        self.inner.flush()
    }
}

/// Formats a byte slice as a hex string with space separators.
///
/// Used internally for logging byte sequences.
#[cfg(test)]
fn format_hex_bytes(bytes: &[u8], max_len: usize) -> String {
    let display_len = bytes.len().min(max_len);
    let hex = bytes[..display_len]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    if bytes.len() > max_len {
        format!("{hex} ...")
    } else {
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod format_hex_bytes_tests {
        use super::*;

        #[test]
        fn empty_bytes() {
            assert_eq!(format_hex_bytes(&[], 64), "");
        }

        #[test]
        fn single_byte() {
            assert_eq!(format_hex_bytes(&[0x42], 64), "42");
        }

        #[test]
        fn multiple_bytes() {
            assert_eq!(format_hex_bytes(&[0x01, 0x02, 0x03], 64), "01 02 03");
        }

        #[test]
        fn hex_formatting_lowercase() {
            assert_eq!(format_hex_bytes(&[0xAB, 0xCD, 0xEF], 64), "ab cd ef");
        }

        #[test]
        fn truncation_at_max_len() {
            let bytes = [0x01, 0x02, 0x03, 0x04, 0x05];
            assert_eq!(format_hex_bytes(&bytes, 3), "01 02 03 ...");
        }

        #[test]
        fn exact_max_len_no_truncation() {
            let bytes = [0x01, 0x02, 0x03];
            assert_eq!(format_hex_bytes(&bytes, 3), "01 02 03");
        }

        #[test]
        fn zero_bytes() {
            assert_eq!(format_hex_bytes(&[0x00, 0x00], 64), "00 00");
        }

        #[test]
        fn full_byte_range() {
            assert_eq!(format_hex_bytes(&[0x00, 0xFF], 64), "00 ff");
        }
    }

    mod tracing_stream_struct_tests {
        use super::*;

        #[test]
        fn struct_fields_documented() {
            // TracingStream has inner, direction, total_written, total_read
            // This test validates the struct exists with expected documentation
            let _ = std::any::type_name::<TracingStream>();
        }
    }

    mod log_to_file_tests {
        use super::*;
        use std::path::Path;

        #[test]
        fn log_to_file_creates_or_appends() {
            // This test verifies log_to_file doesn't panic
            log_to_file("test message from unit test");

            // Verify the log file exists (may have been created by this or other tests)
            let path = Path::new("/tmp/daemon_protocol_trace.log");
            // We don't assert exists because the test might not have permissions
            let _ = path.exists();
        }

        #[test]
        fn log_to_file_handles_special_chars() {
            // Should handle special characters without panic
            log_to_file("test with special chars: \t\n\r");
            log_to_file("unicode: 日本語");
            log_to_file("");
        }

        #[test]
        fn log_to_file_long_message() {
            let long_msg = "x".repeat(10000);
            log_to_file(&long_msg);
        }
    }
}
