//! Whole-line writes to the process diagnostic stream.
//!
//! upstream: log.c:269-427 `rwrite()` formats each message - prefix, text and
//! trailing newline - into one buffer, hands it to `filtered_fwrite()` on the
//! unbuffered `stderr` and calls `fflush()`, so every message reaches fd 2 in
//! a single `write(2)`. The remote shell inherits that same fd
//! (pipe.c:48-97 `piped_child()`), so a message issued as several writes can
//! be torn apart by the remote side's own output.

use std::io::{self, Write};

/// Forwards only complete lines to the inner writer, each batch of complete
/// lines in one `write_all`.
///
/// A line ends at `\n` or `\r`, the latter for progress output, which upstream
/// also terminates with `\r` and flushes. Bytes after the last terminator stay
/// buffered until a later terminator, [`Write::flush`], or drop.
///
/// `write!` and `writeln!` call `write` once per formatted piece; on an
/// unbuffered stream such as [`std::io::Stderr`] each piece would otherwise be
/// its own system call.
pub struct LineAtomicWriter<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> LineAtomicWriter<W> {
    /// Wraps `inner`, which then receives one `write_all` per completed batch.
    pub const fn new(inner: W) -> Self {
        Self {
            inner,
            pending: Vec::new(),
        }
    }

    fn emit_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let result = self.inner.write_all(&self.pending);
        self.pending.clear();
        result
    }
}

impl<W: Write> Write for LineAtomicWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(last) = buf.iter().rposition(|&b| b == b'\n' || b == b'\r') else {
            self.pending.extend_from_slice(buf);
            return Ok(buf.len());
        };
        let (lines, rest) = buf.split_at(last + 1);
        if self.pending.is_empty() {
            self.inner.write_all(lines)?;
        } else {
            self.pending.extend_from_slice(lines);
            self.emit_pending()?;
        }
        self.pending.extend_from_slice(rest);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit_pending()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for LineAtomicWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records each `write` call the wrapper makes on its inner writer.
    #[derive(Default)]
    struct WriteLog {
        calls: Vec<Vec<u8>>,
    }

    impl Write for WriteLog {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.calls.push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_formatted_diagnostic_reaches_the_stream_in_one_write() {
        let mut log = WriteLog::default();
        {
            let mut writer = LineAtomicWriter::new(&mut log);
            let (program, received, role) = ("oc-rsync", 90, "sender");
            writeln!(
                writer,
                "{program}: connection unexpectedly closed ({received} bytes received so far) [{role}]"
            )
            .unwrap();
            assert!(writer.pending.is_empty());
        }
        assert_eq!(
            log.calls,
            vec![
                b"oc-rsync: connection unexpectedly closed (90 bytes received so far) [sender]\n"
                    .to_vec()
            ]
        );
    }

    #[test]
    fn progress_lines_ending_in_carriage_return_are_emitted_whole() {
        let mut log = WriteLog::default();
        {
            let mut writer = LineAtomicWriter::new(&mut log);
            let (bytes, percent) = (1024, 50);
            write!(writer, "{bytes:>10} {percent}%\r").unwrap();
        }
        assert_eq!(log.calls, vec![b"      1024 50%\r".to_vec()]);
    }

    #[test]
    fn a_partial_line_waits_for_its_terminator_then_leaves_with_it() {
        let mut log = WriteLog::default();
        let mut writer = LineAtomicWriter::new(&mut log);
        writer.write_all(b"first\nsecond ").unwrap();
        writer.write_all(b"half\n").unwrap();
        drop(writer);
        assert_eq!(
            log.calls,
            vec![b"first\n".to_vec(), b"second half\n".to_vec()]
        );
    }

    #[test]
    fn flush_and_drop_emit_an_unterminated_tail() {
        let mut log = WriteLog::default();
        {
            let mut writer = LineAtomicWriter::new(&mut log);
            writer.write_all(b"Password: ").unwrap();
            writer.flush().unwrap();
            writer.write_all(b"tail").unwrap();
        }
        assert_eq!(log.calls, vec![b"Password: ".to_vec(), b"tail".to_vec()]);
    }
}
