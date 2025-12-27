use std::fmt::{self, Write as FmtWrite};
use std::io::{self, IoSlice, Write as IoWrite};
use std::str;

use super::{Message, MessageScratch};

impl Message {
    /// Renders the message into an arbitrary [`fmt::Write`] implementor while reusing scratch buffers.
    #[must_use = "formatter writes can fail; propagate errors to preserve upstream diagnostics"]
    pub fn render_to_with_scratch<W: fmt::Write>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> fmt::Result {
        struct Adapter<'a, W>(&'a mut W);

        impl<W: fmt::Write> IoWrite for Adapter<'_, W> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let text = str::from_utf8(buf)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

                self.0
                    .write_str(text)
                    .map_err(|_| io::Error::other("formatter error"))?;

                Ok(buf.len())
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                let mut written = 0usize;

                for buf in bufs {
                    self.write(buf.as_ref())?;
                    written += buf.len();
                }

                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }

            fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
                self.0
                    .write_fmt(fmt)
                    .map_err(|_| io::Error::other("formatter error"))
            }
        }

        let mut adapter = Adapter(writer);
        self.render_to_writer_inner(scratch, &mut adapter, false)
            .map_err(|_| fmt::Error)
    }

    /// Renders the message followed by a newline into an [`fmt::Write`] implementor while reusing scratch buffers.
    #[must_use = "newline rendering can fail; handle formatting errors to retain diagnostics"]
    pub fn render_line_to_with_scratch<W: fmt::Write>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> fmt::Result {
        self.render_to_with_scratch(scratch, writer)?;
        FmtWrite::write_char(writer, '\n')
    }

    /// Collects the rendered message into a [`Vec<u8>`] while reusing caller-provided scratch buffers.
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_bytes_with_scratch(&self, scratch: &mut MessageScratch) -> io::Result<Vec<u8>> {
        self.to_bytes_with_scratch_inner(scratch, false)
    }

    /// Collects the rendered message and a trailing newline into a [`Vec<u8>`] while reusing scratch buffers.
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_line_bytes_with_scratch(&self, scratch: &mut MessageScratch) -> io::Result<Vec<u8>> {
        self.to_bytes_with_scratch_inner(scratch, true)
    }

    /// Streams the rendered message into an [`io::Write`] implementor using caller-provided scratch buffers.
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_to_writer_with_scratch<W: IoWrite>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> io::Result<()> {
        self.render_to_writer_inner(scratch, writer, false)
    }

    /// Writes the rendered message followed by a newline while reusing caller-provided scratch buffers.
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_line_to_writer_with_scratch<W: IoWrite>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> io::Result<()> {
        self.render_to_writer_inner(scratch, writer, true)
    }

    /// Appends the rendered message into the provided buffer while reusing caller-supplied scratch space.
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_to_vec_with_scratch(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
    ) -> io::Result<usize> {
        self.append_to_vec_with_scratch_inner(scratch, buffer, false)
    }

    /// Appends the rendered message followed by a newline into the provided buffer while reusing scratch space.
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_line_to_vec_with_scratch(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
    ) -> io::Result<usize> {
        self.append_to_vec_with_scratch_inner(scratch, buffer, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_to_with_scratch_writes_to_string() {
        let msg = Message::info("test message");
        let mut scratch = MessageScratch::new();
        let mut output = String::new();
        msg.render_to_with_scratch(&mut scratch, &mut output).unwrap();
        assert!(output.contains("test message"));
    }

    #[test]
    fn render_to_with_scratch_does_not_add_newline() {
        let msg = Message::info("test");
        let mut scratch = MessageScratch::new();
        let mut output = String::new();
        msg.render_to_with_scratch(&mut scratch, &mut output).unwrap();
        assert!(!output.ends_with('\n'));
    }

    #[test]
    fn render_line_to_with_scratch_appends_newline() {
        let msg = Message::info("test");
        let mut scratch = MessageScratch::new();
        let mut output = String::new();
        msg.render_line_to_with_scratch(&mut scratch, &mut output).unwrap();
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn to_bytes_with_scratch_returns_vec() {
        let msg = Message::info("hello");
        let mut scratch = MessageScratch::new();
        let bytes = msg.to_bytes_with_scratch(&mut scratch).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn to_bytes_with_scratch_contains_message_text() {
        let msg = Message::info("specific text");
        let mut scratch = MessageScratch::new();
        let bytes = msg.to_bytes_with_scratch(&mut scratch).unwrap();
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains("specific text"));
    }

    #[test]
    fn to_line_bytes_with_scratch_includes_newline() {
        let msg = Message::info("test");
        let mut scratch = MessageScratch::new();
        let bytes = msg.to_line_bytes_with_scratch(&mut scratch).unwrap();
        assert!(bytes.ends_with(b"\n"));
    }

    #[test]
    fn render_to_writer_with_scratch_writes_bytes() {
        let msg = Message::info("writer test");
        let mut scratch = MessageScratch::new();
        let mut buffer = Vec::new();
        msg.render_to_writer_with_scratch(&mut scratch, &mut buffer).unwrap();
        assert!(!buffer.is_empty());
    }

    #[test]
    fn render_line_to_writer_with_scratch_includes_newline() {
        let msg = Message::info("line test");
        let mut scratch = MessageScratch::new();
        let mut buffer = Vec::new();
        msg.render_line_to_writer_with_scratch(&mut scratch, &mut buffer).unwrap();
        assert!(buffer.ends_with(b"\n"));
    }

    #[test]
    fn append_to_vec_with_scratch_extends_buffer() {
        let msg = Message::info("append test");
        let mut scratch = MessageScratch::new();
        let mut buffer = b"prefix:".to_vec();
        let prefix_len = buffer.len();
        msg.append_to_vec_with_scratch(&mut scratch, &mut buffer).unwrap();
        assert!(buffer.len() > prefix_len);
    }

    #[test]
    fn append_to_vec_with_scratch_preserves_prefix() {
        let msg = Message::info("test");
        let mut scratch = MessageScratch::new();
        let mut buffer = b"prefix:".to_vec();
        msg.append_to_vec_with_scratch(&mut scratch, &mut buffer).unwrap();
        assert_eq!(&buffer[..7], b"prefix:");
    }

    #[test]
    fn append_line_to_vec_with_scratch_includes_newline() {
        let msg = Message::info("line");
        let mut scratch = MessageScratch::new();
        let mut buffer = Vec::new();
        msg.append_line_to_vec_with_scratch(&mut scratch, &mut buffer).unwrap();
        assert!(buffer.ends_with(b"\n"));
    }

    #[test]
    fn scratch_can_be_reused_for_multiple_messages() {
        let msg1 = Message::info("first");
        let msg2 = Message::warning("second");
        let mut scratch = MessageScratch::new();

        let bytes1 = msg1.to_bytes_with_scratch(&mut scratch).unwrap();
        let bytes2 = msg2.to_bytes_with_scratch(&mut scratch).unwrap();

        assert!(!bytes1.is_empty());
        assert!(!bytes2.is_empty());
    }

    #[test]
    fn append_to_vec_with_scratch_returns_appended_length() {
        let msg = Message::info("test");
        let mut scratch = MessageScratch::new();
        let mut buffer = Vec::new();
        let appended = msg.append_to_vec_with_scratch(&mut scratch, &mut buffer).unwrap();
        assert_eq!(appended, buffer.len());
    }

    #[test]
    fn append_line_to_vec_with_scratch_returns_length_with_newline() {
        let msg = Message::info("test");
        let mut scratch = MessageScratch::new();
        let mut buffer = Vec::new();
        let appended = msg.append_line_to_vec_with_scratch(&mut scratch, &mut buffer).unwrap();
        assert_eq!(appended, buffer.len());
        assert!(buffer.ends_with(b"\n"));
    }
}
