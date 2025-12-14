//! Protocol debugging trace system for daemon mode
//!
//! Since daemon mode closes stderr, we need file-based logging to debug
//! protocol issues. This module provides wrappers that log all I/O operations
//! to trace files.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

static TRACE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Configuration for protocol tracing
#[derive(Clone)]
pub struct TraceConfig {
    /// Base directory for trace files
    pub trace_dir: String,
    /// Prefix for trace file names (e.g., "client", "server")
    pub prefix: String,
    /// Whether to enable tracing
    pub enabled: bool,
}

impl TraceConfig {
    /// Create a disabled trace configuration
    pub fn disabled() -> Self {
        Self {
            trace_dir: "/tmp/rsync-trace".to_string(),
            prefix: "trace".to_string(),
            enabled: false,
        }
    }

    /// Create an enabled trace configuration with the given prefix
    pub fn enabled(prefix: &str) -> Self {
        Self {
            trace_dir: "/tmp/rsync-trace".to_string(),
            prefix: prefix.to_string(),
            enabled: true,
        }
    }
}

/// Wrapper for Read that logs all bytes read
pub struct TracingReader<R> {
    inner: R,
    config: TraceConfig,
    sequence: u64,
}

impl<R: Read> TracingReader<R> {
    /// Create a new tracing reader that wraps the given reader
    pub fn new(inner: R, config: TraceConfig) -> Self {
        let sequence = TRACE_COUNTER.fetch_add(1, Ordering::SeqCst);

        if config.enabled {
            // Create trace directory if it doesn't exist
            let _ = std::fs::create_dir_all(&config.trace_dir);

            // Write header file
            let header_path = format!(
                "{}/{}_read_{:04}_header.txt",
                config.trace_dir, config.prefix, sequence
            );
            let _ = std::fs::write(
                &header_path,
                format!("Read trace started at {:?}\n", std::time::SystemTime::now()),
            );
        }

        Self {
            inner,
            config,
            sequence,
        }
    }

    fn log_read(&self, buf: &[u8], result: &io::Result<usize>) {
        if !self.config.enabled {
            return;
        }

        let trace_path = format!(
            "{}/{}_read_{:04}.log",
            self.config.trace_dir, self.config.prefix, self.sequence
        );

        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
        {
            Ok(f) => f,
            Err(_) => return,
        };

        match result {
            Ok(n) => {
                let _ = writeln!(file, "[READ] {n} bytes:");
                let _ = writeln!(file, "  Hex: {}", hex_dump(&buf[..*n]));
                let _ = writeln!(file, "  ASCII: {}", ascii_dump(&buf[..*n]));
                let _ = writeln!(file);
            }
            Err(e) => {
                let _ = writeln!(file, "[READ ERROR] {e}");
            }
        }
    }
}

impl<R: Read> Read for TracingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = self.inner.read(buf);
        if let Ok(n) = result {
            self.log_read(&buf[..n], &Ok(n));
        } else {
            self.log_read(buf, &result);
        }
        result
    }
}

/// Wrapper for Write that logs all bytes written
pub struct TracingWriter<W> {
    inner: W,
    config: TraceConfig,
    sequence: u64,
}

impl<W: Write> TracingWriter<W> {
    /// Create a new tracing writer that wraps the given writer
    pub fn new(inner: W, config: TraceConfig) -> Self {
        let sequence = TRACE_COUNTER.fetch_add(1, Ordering::SeqCst);

        if config.enabled {
            // Create trace directory if it doesn't exist
            let _ = std::fs::create_dir_all(&config.trace_dir);

            // Write header file
            let header_path = format!(
                "{}/{}_write_{:04}_header.txt",
                config.trace_dir, config.prefix, sequence
            );
            let _ = std::fs::write(
                &header_path,
                format!(
                    "Write trace started at {:?}\n",
                    std::time::SystemTime::now()
                ),
            );
        }

        Self {
            inner,
            config,
            sequence,
        }
    }

    fn log_write(&self, buf: &[u8], result: &io::Result<usize>) {
        if !self.config.enabled {
            return;
        }

        let trace_path = format!(
            "{}/{}_write_{:04}.log",
            self.config.trace_dir, self.config.prefix, self.sequence
        );

        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
        {
            Ok(f) => f,
            Err(_) => return,
        };

        match result {
            Ok(n) => {
                let _ = writeln!(file, "[WRITE] {n} bytes:");
                let _ = writeln!(file, "  Hex: {}", hex_dump(&buf[..*n]));
                let _ = writeln!(file, "  ASCII: {}", ascii_dump(&buf[..*n]));
                let _ = writeln!(file);
            }
            Err(e) => {
                let _ = writeln!(file, "[WRITE ERROR] {e}");
            }
        }
    }
}

impl<W: Write> Write for TracingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(buf);
        self.log_write(buf, &result);
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Create a hex dump string from bytes
fn hex_dump(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .chunks(16)
        .map(|chunk| chunk.join(" "))
        .collect::<Vec<_>>()
        .join("\n     ")
}

/// Create an ASCII dump string from bytes (show printable, '.' for non-printable)
fn ascii_dump(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect::<String>()
        .chars()
        .collect::<Vec<_>>()
        .chunks(16)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n       ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_dump() {
        let data = b"Hello, World!";
        let dump = hex_dump(data);
        assert!(dump.contains("48 65 6c 6c 6f")); // "Hello"
    }

    #[test]
    fn test_ascii_dump() {
        let data = b"Hello\x00\x01\x02";
        let dump = ascii_dump(data);
        assert_eq!(dump, "Hello...");
    }
}
