use std::fmt;
use std::io::{self, Write as IoWrite};

use super::{Message, MessageScratch};

impl Message {
    /// Renders the message into an arbitrary [`fmt::Write`] implementation.
    #[inline]
    #[must_use = "formatter writes can fail; propagate errors to preserve upstream diagnostics"]
    pub fn render_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        MessageScratch::with_thread_local(|scratch| self.render_to_with_scratch(scratch, writer))
    }

    /// Renders the message followed by a newline into an arbitrary [`fmt::Write`] implementor.
    #[inline]
    #[must_use = "newline rendering can fail; handle formatting errors to retain diagnostics"]
    pub fn render_line_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        MessageScratch::with_thread_local(|scratch| {
            self.render_line_to_with_scratch(scratch, writer)
        })
    }

    /// Returns the rendered message as a [`Vec<u8>`].
    #[inline]
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        MessageScratch::with_thread_local(|scratch| self.to_bytes_with_scratch(scratch))
    }

    /// Returns the rendered message followed by a newline as a [`Vec<u8>`].
    #[inline]
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_line_bytes(&self) -> io::Result<Vec<u8>> {
        MessageScratch::with_thread_local(|scratch| self.to_line_bytes_with_scratch(scratch))
    }

    /// Writes the rendered message into an [`io::Write`] implementor.
    #[inline]
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        MessageScratch::with_thread_local(|scratch| {
            self.render_to_writer_with_scratch(scratch, writer)
        })
    }

    /// Writes the rendered message followed by a newline into an [`io::Write`] implementor.
    #[inline]
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_line_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        MessageScratch::with_thread_local(|scratch| {
            self.render_line_to_writer_with_scratch(scratch, writer)
        })
    }

    /// Appends the rendered message into the provided byte buffer.
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_to_vec(&self, buffer: &mut Vec<u8>) -> io::Result<usize> {
        MessageScratch::with_thread_local(|scratch| {
            self.append_to_vec_with_scratch(scratch, buffer)
        })
    }

    /// Appends the rendered message followed by a newline into the provided buffer.
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_line_to_vec(&self, buffer: &mut Vec<u8>) -> io::Result<usize> {
        MessageScratch::with_thread_local(|scratch| {
            self.append_line_to_vec_with_scratch(scratch, buffer)
        })
    }

    pub(super) fn render_to_writer_inner<W: IoWrite>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
        include_newline: bool,
    ) -> io::Result<()> {
        let segments = self.as_segments(scratch, include_newline);
        segments.write_to(writer)
    }

    pub(super) fn to_bytes_with_scratch_inner(
        &self,
        scratch: &mut MessageScratch,
        include_newline: bool,
    ) -> io::Result<Vec<u8>> {
        let segments = self.as_segments(scratch, include_newline);
        let mut buffer = Vec::new();
        let _ = segments.extend_vec(&mut buffer)?;
        Ok(buffer)
    }

    pub(super) fn append_to_vec_with_scratch_inner(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
        include_newline: bool,
    ) -> io::Result<usize> {
        let segments = self.as_segments(scratch, include_newline);
        segments.extend_vec(buffer)
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.render_to(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_to_writes_to_string() {
        let msg = Message::info("test message");
        let mut output = String::new();
        msg.render_to(&mut output).unwrap();
        assert!(output.contains("test message"));
    }

    #[test]
    fn render_line_to_appends_newline() {
        let msg = Message::info("test");
        let mut output = String::new();
        msg.render_line_to(&mut output).unwrap();
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn to_bytes_returns_vec() {
        let msg = Message::info("hello");
        let bytes = msg.to_bytes().unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn to_bytes_contains_message_text() {
        let msg = Message::info("specific text");
        let bytes = msg.to_bytes().unwrap();
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains("specific text"));
    }

    #[test]
    fn to_line_bytes_includes_newline() {
        let msg = Message::info("test");
        let bytes = msg.to_line_bytes().unwrap();
        assert!(bytes.ends_with(b"\n"));
    }

    #[test]
    fn render_to_writer_writes_bytes() {
        let msg = Message::info("writer test");
        let mut buffer = Vec::new();
        msg.render_to_writer(&mut buffer).unwrap();
        assert!(!buffer.is_empty());
    }

    #[test]
    fn render_line_to_writer_includes_newline() {
        let msg = Message::info("line test");
        let mut buffer = Vec::new();
        msg.render_line_to_writer(&mut buffer).unwrap();
        assert!(buffer.ends_with(b"\n"));
    }

    #[test]
    fn append_to_vec_extends_buffer() {
        let msg = Message::info("append test");
        let mut buffer = b"prefix:".to_vec();
        let prefix_len = buffer.len();
        msg.append_to_vec(&mut buffer).unwrap();
        assert!(buffer.len() > prefix_len);
    }

    #[test]
    fn append_to_vec_preserves_prefix() {
        let msg = Message::info("test");
        let mut buffer = b"prefix:".to_vec();
        msg.append_to_vec(&mut buffer).unwrap();
        assert_eq!(&buffer[..7], b"prefix:");
    }

    #[test]
    fn append_line_to_vec_includes_newline() {
        let msg = Message::info("line");
        let mut buffer = Vec::new();
        msg.append_line_to_vec(&mut buffer).unwrap();
        assert!(buffer.ends_with(b"\n"));
    }

    #[test]
    fn display_trait_works() {
        let msg = Message::info("display test");
        let output = format!("{msg}");
        assert!(output.contains("display test"));
    }

    #[test]
    fn display_does_not_include_newline() {
        let msg = Message::info("test");
        let output = format!("{msg}");
        assert!(!output.ends_with('\n'));
    }

    #[test]
    fn error_message_includes_code() {
        let msg = Message::error(42, "error text");
        let bytes = msg.to_bytes().unwrap();
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains("error text"));
    }

    #[test]
    fn warning_message_renders() {
        let msg = Message::warning("warning text");
        let bytes = msg.to_bytes().unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn append_to_vec_returns_appended_length() {
        let msg = Message::info("test");
        let mut buffer = Vec::new();
        let appended = msg.append_to_vec(&mut buffer).unwrap();
        assert_eq!(appended, buffer.len());
    }

    #[test]
    fn append_line_to_vec_returns_length_with_newline() {
        let msg = Message::info("test");
        let mut buffer = Vec::new();
        let appended = msg.append_line_to_vec(&mut buffer).unwrap();
        assert_eq!(appended, buffer.len());
        assert!(buffer.ends_with(b"\n"));
    }
}
