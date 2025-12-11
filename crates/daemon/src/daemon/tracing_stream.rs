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
