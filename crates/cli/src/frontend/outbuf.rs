#![deny(unsafe_code)]

use std::ffi::OsStr;
use std::io::{self, BufWriter, LineWriter, Write};

use oc_rsync_core::{
    message::{Message, Role},
    rsync_error,
};

/// Buffering modes supported by the `--outbuf` command-line option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutbufMode {
    /// Disable buffering and flush the underlying stream after every write.
    None,
    /// Flush the underlying stream whenever a newline byte is written.
    Line,
    /// Use block buffering for the underlying stream.
    Block,
}

/// Parses the value supplied via `--outbuf`.
///
/// The implementation mirrors upstream rsync's behaviour by accepting the
/// leading character of the provided string (case-insensitive) and mapping
/// `N`/`U` to no buffering, `L` to line buffering, and `B`/`F` to block
/// buffering. Any other leading character results in an error diagnostic.
pub(crate) fn parse_outbuf_mode(value: &OsStr) -> Result<OutbufMode, Message> {
    if value.is_empty() {
        return Err(invalid_outbuf_message());
    }

    let value_string = value.to_string_lossy();
    let mut chars = value_string.chars();
    let mode = chars.next().ok_or_else(invalid_outbuf_message)?;
    match mode.to_ascii_uppercase() {
        'N' | 'U' => Ok(OutbufMode::None),
        'L' => Ok(OutbufMode::Line),
        'B' | 'F' => Ok(OutbufMode::Block),
        _ => Err(invalid_outbuf_message()),
    }
}

fn invalid_outbuf_message() -> Message {
    rsync_error!(1, "Invalid --outbuf setting -- specify N, L, or B.").with_role(Role::Client)
}

enum OutbufAdapterInner<'a, W: Write> {
    None(NoBufferWriter<'a, W>),
    Line(LineWriter<&'a mut W>),
    Block(BufWriter<&'a mut W>),
}

/// Writer adapter that applies the requested `--outbuf` mode to an existing writer.
pub(crate) struct OutbufAdapter<'a, W: Write> {
    inner: OutbufAdapterInner<'a, W>,
}

impl<'a, W: Write> OutbufAdapter<'a, W> {
    /// Creates a new adapter that wraps `writer` with the buffering semantics
    /// specified by `mode`.
    #[must_use]
    pub(crate) fn new(writer: &'a mut W, mode: OutbufMode) -> Self {
        let inner = match mode {
            OutbufMode::None => OutbufAdapterInner::None(NoBufferWriter { inner: writer }),
            OutbufMode::Line => OutbufAdapterInner::Line(LineWriter::new(writer)),
            OutbufMode::Block => OutbufAdapterInner::Block(BufWriter::new(writer)),
        };
        Self { inner }
    }

    /// Flushes any buffered data to the underlying writer.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        match &mut self.inner {
            OutbufAdapterInner::None(writer) => writer.flush(),
            OutbufAdapterInner::Line(writer) => writer.flush(),
            OutbufAdapterInner::Block(writer) => writer.flush(),
        }
    }
}

impl<W: Write> Write for OutbufAdapter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.inner {
            OutbufAdapterInner::None(writer) => writer.write(buf),
            OutbufAdapterInner::Line(writer) => writer.write(buf),
            OutbufAdapterInner::Block(writer) => writer.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        OutbufAdapter::flush(self)
    }
}

struct NoBufferWriter<'a, W: Write> {
    inner: &'a mut W,
}

impl<W: Write> Write for NoBufferWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.inner.flush()?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{OutbufAdapter, OutbufMode, parse_outbuf_mode};
    use std::ffi::OsStr;
    use std::io::Write;

    #[test]
    fn parse_accepts_uppercase_variants() {
        assert_eq!(
            parse_outbuf_mode(OsStr::new("N")).unwrap(),
            OutbufMode::None
        );
        assert_eq!(
            parse_outbuf_mode(OsStr::new("L")).unwrap(),
            OutbufMode::Line
        );
        assert_eq!(
            parse_outbuf_mode(OsStr::new("B")).unwrap(),
            OutbufMode::Block
        );
    }

    #[test]
    fn parse_accepts_lowercase_variants() {
        assert_eq!(
            parse_outbuf_mode(OsStr::new("none")).unwrap(),
            OutbufMode::None
        );
        assert_eq!(
            parse_outbuf_mode(OsStr::new("line")).unwrap(),
            OutbufMode::Line
        );
        assert_eq!(
            parse_outbuf_mode(OsStr::new("block")).unwrap(),
            OutbufMode::Block
        );
    }

    #[test]
    fn parse_accepts_full_synonyms() {
        assert_eq!(
            parse_outbuf_mode(OsStr::new("full")).unwrap(),
            OutbufMode::Block
        );
        assert_eq!(
            parse_outbuf_mode(OsStr::new("unbuffered")).unwrap(),
            OutbufMode::None
        );
    }

    #[test]
    fn parse_rejects_unknown_values() {
        let error =
            parse_outbuf_mode(OsStr::new("x")).expect_err("unknown mode should be rejected");
        let rendered = error.to_string();
        assert!(rendered.contains("Invalid --outbuf setting"));
    }

    #[test]
    fn adapter_flushes_without_mode_changes() {
        let mut buffer = Vec::new();
        {
            let mut adapter = OutbufAdapter::new(&mut buffer, OutbufMode::Block);
            adapter.write_all(b"payload").unwrap();
            adapter.flush().unwrap();
        }
        assert_eq!(buffer, b"payload");
    }
}
