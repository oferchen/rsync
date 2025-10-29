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
