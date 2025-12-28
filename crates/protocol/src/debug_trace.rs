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
            trace_dir: "/tmp/rsync-trace".to_owned(),
            prefix: "trace".to_owned(),
            enabled: false,
        }
    }

    /// Create an enabled trace configuration with the given prefix
    pub fn enabled(prefix: &str) -> Self {
        Self {
            trace_dir: "/tmp/rsync-trace".to_owned(),
            prefix: prefix.to_owned(),
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

        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
        else {
            return;
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

        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
        else {
            return;
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

/// Create a hex dump string from bytes.
///
/// Single-pass implementation that pre-allocates capacity to avoid
/// intermediate Vec allocations.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write;

    if bytes.is_empty() {
        return String::new();
    }

    // Each byte = 2 hex chars + 1 space, plus newlines every 16 bytes
    let num_lines = bytes.len().div_ceil(16);
    let capacity = bytes.len() * 3 + num_lines * 5;
    let mut result = String::with_capacity(capacity);

    for (i, chunk) in bytes.chunks(16).enumerate() {
        if i > 0 {
            result.push_str("\n     ");
        }
        for (j, b) in chunk.iter().enumerate() {
            if j > 0 {
                result.push(' ');
            }
            let _ = write!(result, "{b:02x}");
        }
    }
    result
}

/// Create an ASCII dump string from bytes (show printable, '.' for non-printable).
///
/// Single-pass implementation that avoids intermediate allocations.
fn ascii_dump(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    // Each byte = 1 char, plus newline+indent every 16 bytes
    let num_lines = bytes.len().div_ceil(16);
    let capacity = bytes.len() + num_lines * 7;
    let mut result = String::with_capacity(capacity);

    for (i, chunk) in bytes.chunks(16).enumerate() {
        if i > 0 {
            result.push_str("\n       ");
        }
        for &b in chunk {
            result.push(if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_hex_dump() {
        let data = b"Hello, World!";
        let dump = hex_dump(data);
        assert!(dump.contains("48 65 6c 6c 6f")); // "Hello"
    }

    #[test]
    fn test_hex_dump_empty() {
        let data: &[u8] = &[];
        let dump = hex_dump(data);
        assert_eq!(dump, "");
    }

    #[test]
    fn test_hex_dump_longer_than_16_bytes() {
        let data = b"This is a longer string with more than 16 bytes";
        let dump = hex_dump(data);
        // Should contain line break after 16 bytes
        assert!(dump.contains('\n'));
    }

    #[test]
    fn test_ascii_dump() {
        let data = b"Hello\x00\x01\x02";
        let dump = ascii_dump(data);
        assert_eq!(dump, "Hello...");
    }

    #[test]
    fn test_ascii_dump_empty() {
        let data: &[u8] = &[];
        let dump = ascii_dump(data);
        assert_eq!(dump, "");
    }

    #[test]
    fn test_ascii_dump_all_printable() {
        let data = b"ABC123";
        let dump = ascii_dump(data);
        assert_eq!(dump, "ABC123");
    }

    #[test]
    fn test_ascii_dump_all_non_printable() {
        let data = &[0x00, 0x01, 0x02, 0x1F];
        let dump = ascii_dump(data);
        assert_eq!(dump, "....");
    }

    #[test]
    fn test_ascii_dump_boundary_chars() {
        // Test boundary characters: 0x1F (non-printable), 0x20 (space), 0x7E (~), 0x7F (non-printable)
        let data = &[0x1F, 0x20, 0x7E, 0x7F];
        let dump = ascii_dump(data);
        assert_eq!(dump, ". ~.");
    }

    #[test]
    fn test_ascii_dump_longer_than_16_chars() {
        let data = b"This is longer than sixteen bytes easily";
        let dump = ascii_dump(data);
        // Should contain line break after 16 chars
        assert!(dump.contains('\n'));
    }

    #[test]
    fn trace_config_disabled_defaults() {
        let config = TraceConfig::disabled();
        assert!(!config.enabled);
        assert_eq!(config.trace_dir, "/tmp/rsync-trace");
        assert_eq!(config.prefix, "trace");
    }

    #[test]
    fn trace_config_enabled_with_prefix() {
        let config = TraceConfig::enabled("server");
        assert!(config.enabled);
        assert_eq!(config.prefix, "server");
        assert_eq!(config.trace_dir, "/tmp/rsync-trace");
    }

    #[test]
    fn trace_config_clone() {
        let config = TraceConfig::enabled("test");
        let cloned = config.clone();
        assert_eq!(config.enabled, cloned.enabled);
        assert_eq!(config.prefix, cloned.prefix);
        assert_eq!(config.trace_dir, cloned.trace_dir);
    }

    #[test]
    fn tracing_reader_disabled_reads_normally() {
        let data = b"test data";
        let cursor = Cursor::new(data.to_vec());
        let mut reader = TracingReader::new(cursor, TraceConfig::disabled());

        let mut buf = [0u8; 9];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"test data");
    }

    #[test]
    fn tracing_reader_enabled_reads_normally() {
        let data = b"test data";
        let cursor = Cursor::new(data.to_vec());
        let config = TraceConfig {
            trace_dir: std::env::temp_dir()
                .join("rsync-trace-test")
                .to_string_lossy()
                .into_owned(),
            prefix: "test_reader".to_owned(),
            enabled: true,
        };
        let mut reader = TracingReader::new(cursor, config);

        let mut buf = [0u8; 9];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"test data");
    }

    #[test]
    fn tracing_reader_partial_read() {
        let data = b"test data with more";
        let cursor = Cursor::new(data.to_vec());
        let mut reader = TracingReader::new(cursor, TraceConfig::disabled());

        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"test");
    }

    #[test]
    fn tracing_writer_disabled_writes_normally() {
        let mut output = Vec::new();
        {
            let mut writer = TracingWriter::new(&mut output, TraceConfig::disabled());
            writer.write_all(b"test data").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(output, b"test data");
    }

    #[test]
    fn tracing_writer_enabled_writes_normally() {
        let mut output = Vec::new();
        let config = TraceConfig {
            trace_dir: std::env::temp_dir()
                .join("rsync-trace-test")
                .to_string_lossy()
                .into_owned(),
            prefix: "test_writer".to_owned(),
            enabled: true,
        };
        {
            let mut writer = TracingWriter::new(&mut output, config);
            writer.write_all(b"test data").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(output, b"test data");
    }

    #[test]
    fn tracing_writer_flush() {
        let mut output = Vec::new();
        let mut writer = TracingWriter::new(&mut output, TraceConfig::disabled());
        writer.write_all(b"data").unwrap();
        assert!(writer.flush().is_ok());
    }

    #[test]
    fn tracing_reader_sequence_increments() {
        let config = TraceConfig::disabled();
        let cursor1 = Cursor::new(vec![1, 2, 3]);
        let cursor2 = Cursor::new(vec![4, 5, 6]);

        let reader1 = TracingReader::new(cursor1, config.clone());
        let reader2 = TracingReader::new(cursor2, config);

        // Sequences should be different (incremented)
        assert_ne!(reader1.sequence, reader2.sequence);
    }

    #[test]
    fn tracing_writer_sequence_increments() {
        let config = TraceConfig::disabled();
        let mut output1 = Vec::new();
        let mut output2 = Vec::new();

        let writer1 = TracingWriter::new(&mut output1, config.clone());
        let writer2 = TracingWriter::new(&mut output2, config);

        // Sequences should be different (incremented)
        assert_ne!(writer1.sequence, writer2.sequence);
    }
}
