use super::MessageSink;
use crate::escape::{self, EscapeStyle};
use crate::line_mode::LineMode;
use core::message::{Message, MessageSegments};
use std::borrow::Borrow;
use std::io::{self, Write};

impl<W> MessageSink<W>
where
    W: Write,
{
    fn render_message(&mut self, message: &Message, append_newline: bool) -> io::Result<()> {
        // The prefix, role and trailer are fixed printable ASCII, so only the
        // text can need escaping; a clean text keeps the vectored, copy-free
        // render below.
        let style = self.escape_style();
        if escape::may_need_escaping(message.text().as_bytes(), style) {
            return self.render_escaped(message, append_newline, style);
        }
        if append_newline {
            message.render_line_to_writer_with_scratch(&mut self.scratch, &mut self.writer)
        } else {
            message.render_to_writer_with_scratch(&mut self.scratch, &mut self.writer)
        }
    }

    /// Writes `line` and a newline in one write, escaped with
    /// [`escape_style`](Self::escape_style).
    ///
    /// For a pre-formatted diagnostic that is not a [`Message`], such as the
    /// unprefixed `rprintf(FERROR, ...)` cause line ahead of `log_exit()`'s
    /// trailer: upstream's `rwrite()` escapes it like any other line
    /// (log.c:476, log.c:499). A line with nothing to escape is written as is,
    /// without building a copy.
    ///
    /// # Errors
    ///
    /// Any error writing to the underlying writer.
    pub fn write_escaped_line(&mut self, line: &str) -> io::Result<()> {
        let style = self.escape_style();
        if !escape::may_need_escaping(line.as_bytes(), style) {
            return writeln!(self.writer, "{line}");
        }
        let mut out = escape::escape_for_output(line.as_bytes(), style);
        out.push(b'\n');
        self.writer.write_all(&out)
    }

    /// Renders `message` as one escaped terminal line.
    ///
    /// upstream: log.c:460 `rwrite()` peels one trailing newline and writes it
    /// raw after `filtered_fwrite()` has escaped the rest of the line, so a
    /// newline inside the text prints as `\#012` and cannot forge a line.
    fn render_escaped(
        &mut self,
        message: &Message,
        append_newline: bool,
        style: EscapeStyle,
    ) -> io::Result<()> {
        let mut rendered = Vec::new();
        message.append_to_vec_with_scratch(&mut self.scratch, &mut rendered)?;
        let mut body = rendered.as_slice();
        let mut trailing_newline = append_newline;
        if let Some(rest) = body.strip_suffix(b"\n") {
            body = rest;
            trailing_newline = true;
        }
        let mut line = escape::escape_for_output(body, style);
        if trailing_newline {
            line.push(b'\n');
        }
        self.writer.write_all(&line)
    }

    /// Writes a single message using the sink's current [`LineMode`].
    ///
    /// Accepts borrowed or owned `Message` values via [`Borrow<Message>`] so
    /// call sites can forward diagnostics without cloning.
    pub fn write<M>(&mut self, message: M) -> io::Result<()>
    where
        M: Borrow<Message>,
    {
        let branded = message.borrow().clone().with_brand(self.brand);
        self.render_message(&branded, self.line_mode.append_newline())
    }

    /// Writes `message` using an explicit [`LineMode`] without mutating the sink.
    ///
    /// Mirrors [`write`](Self::write) but lets callers override the newline
    /// behaviour for a single message (for example, progress indicators that
    /// are overwritten in-place).
    pub fn write_with_mode<M>(&mut self, message: M, line_mode: LineMode) -> io::Result<()>
    where
        M: Borrow<Message>,
    {
        let branded = message.borrow().clone().with_brand(self.brand);
        self.render_message(&branded, line_mode.append_newline())
    }

    /// Streams pre-rendered [`MessageSegments`] into the underlying writer.
    ///
    /// The helper allows callers that already rendered a `Message` into
    /// vectored slices (for example, to inspect or buffer them) to forward the
    /// segments without requesting another render. The sink honours its
    /// configured [`LineMode`] when deciding whether to append a trailing
    /// newline; callers must indicate whether `segments` already include a
    /// newline slice via the `segments_include_newline` flag. Passing `false`
    /// matches the common case of invoking [`Message::as_segments`] with
    /// `include_newline` set to `false`.
    pub fn write_segments(
        &mut self,
        segments: &MessageSegments<'_>,
        segments_include_newline: bool,
    ) -> io::Result<()> {
        self.write_segments_with_mode(segments, self.line_mode, segments_include_newline)
    }

    /// Writes pre-rendered [`MessageSegments`] using an explicit [`LineMode`].
    ///
    /// This mirrors [`write_segments`](Self::write_segments) but allows callers
    /// to override the newline behaviour for a single emission. The
    /// `segments_include_newline` flag indicates whether the supplied segments
    /// already contain a terminating newline (for example when rendered via
    /// [`Message::as_segments`] with `include_newline = true`). When the flag is
    /// `false` and the selected [`LineMode`] appends newlines, the sink writes
    /// the trailing newline after streaming the segments.
    pub fn write_segments_with_mode(
        &mut self,
        segments: &MessageSegments<'_>,
        line_mode: LineMode,
        segments_include_newline: bool,
    ) -> io::Result<()> {
        segments.write_to(&mut self.writer)?;

        if line_mode.append_newline() && !segments_include_newline {
            self.writer.write_all(b"\n")?;
        }

        Ok(())
    }

    /// Writes each message from the iterator to the underlying writer.
    ///
    /// Items may be borrowed or owned `Message` values via
    /// [`Borrow<Message>`], so callers can pass collections such as
    /// [`Vec<Message>`] or arrays directly.
    pub fn write_all<I, M>(&mut self, messages: I) -> io::Result<()>
    where
        I: IntoIterator<Item = M>,
        M: Borrow<Message>,
    {
        let append_newline = self.line_mode.append_newline();
        for message in messages {
            let branded = message.borrow().clone().with_brand(self.brand);
            self.render_message(&branded, append_newline)?;
        }
        Ok(())
    }

    /// Writes each message from the iterator using the provided [`LineMode`].
    ///
    /// Mirrors [`write_all`](Self::write_all) but lets callers batch messages
    /// with a specific newline mode without mutating the sink's configuration.
    pub fn write_all_with_mode<I, M>(&mut self, messages: I, line_mode: LineMode) -> io::Result<()>
    where
        I: IntoIterator<Item = M>,
        M: Borrow<Message>,
    {
        let append_newline = line_mode.append_newline();
        for message in messages {
            let branded = message.borrow().clone().with_brand(self.brand);
            self.render_message(&branded, append_newline)?;
        }
        Ok(())
    }

    /// Flushes the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::message::MessageScratch;

    fn make_sink() -> MessageSink<Vec<u8>> {
        MessageSink::new(Vec::new())
    }

    #[test]
    fn write_writes_message_to_buffer() {
        let mut sink = make_sink();
        let msg = Message::info("test message");
        sink.write(&msg).unwrap();
        assert!(!sink.writer().is_empty());
    }

    #[test]
    fn write_appends_newline_in_default_mode() {
        let mut sink = make_sink();
        let msg = Message::info("test");
        sink.write(&msg).unwrap();
        assert!(sink.writer().ends_with(b"\n"));
    }

    #[test]
    fn write_with_mode_uses_provided_mode() {
        let mut sink = make_sink();
        let msg = Message::info("test");
        sink.write_with_mode(&msg, LineMode::WithoutNewline)
            .unwrap();
        assert!(!sink.writer().ends_with(b"\n"));
    }

    #[test]
    fn write_with_mode_with_newline_appends_newline() {
        let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        let msg = Message::info("test");
        sink.write_with_mode(&msg, LineMode::WithNewline).unwrap();
        assert!(sink.writer().ends_with(b"\n"));
    }

    #[test]
    fn write_segments_writes_to_buffer() {
        let mut sink = make_sink();
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        sink.write_segments(&segments, false).unwrap();
        assert!(!sink.writer().is_empty());
    }

    #[test]
    fn write_segments_appends_newline_when_needed() {
        let mut sink = make_sink();
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        sink.write_segments(&segments, false).unwrap();
        assert!(sink.writer().ends_with(b"\n"));
    }

    #[test]
    fn write_segments_no_double_newline() {
        let mut sink = make_sink();
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, true);
        sink.write_segments(&segments, true).unwrap();
        let buffer = sink.writer();
        assert!(buffer.ends_with(b"\n"));
        assert!(!buffer.ends_with(b"\n\n"));
    }

    #[test]
    fn write_segments_with_mode_uses_provided_mode() {
        let mut sink = make_sink();
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        sink.write_segments_with_mode(&segments, LineMode::WithoutNewline, false)
            .unwrap();
        assert!(!sink.writer().ends_with(b"\n"));
    }

    #[test]
    fn write_all_writes_multiple_messages() {
        let mut sink = make_sink();
        let messages = vec![
            Message::info("first"),
            Message::info("second"),
            Message::info("third"),
        ];
        sink.write_all(&messages).unwrap();
        let output = String::from_utf8_lossy(sink.writer());
        assert!(output.contains("first"));
        assert!(output.contains("second"));
        assert!(output.contains("third"));
    }

    #[test]
    fn write_all_appends_newline_to_each() {
        let mut sink = make_sink();
        let messages = vec![Message::info("a"), Message::info("b")];
        sink.write_all(&messages).unwrap();
        let output = sink.writer();
        let newline_count = output.iter().filter(|&&b| b == b'\n').count();
        assert_eq!(newline_count, 2);
    }

    #[test]
    fn write_all_with_mode_uses_provided_mode() {
        let mut sink = make_sink();
        let messages = vec![Message::info("a"), Message::info("b")];
        sink.write_all_with_mode(&messages, LineMode::WithoutNewline)
            .unwrap();
        let output = sink.writer();
        let newline_count = output.iter().filter(|&&b| b == b'\n').count();
        assert_eq!(newline_count, 0);
    }

    #[test]
    fn write_all_empty_iterator_succeeds() {
        let mut sink = make_sink();
        let messages: Vec<Message> = vec![];
        let result = sink.write_all(messages);
        assert!(result.is_ok());
        assert!(sink.writer().is_empty());
    }

    #[test]
    fn flush_succeeds() {
        let mut sink = make_sink();
        let result = sink.flush();
        assert!(result.is_ok());
    }

    #[test]
    fn write_accepts_owned_message() {
        let mut sink = make_sink();
        let msg = Message::info("owned");
        sink.write(msg).unwrap();
        assert!(!sink.writer().is_empty());
    }

    #[test]
    fn write_accepts_borrowed_message() {
        let mut sink = make_sink();
        let msg = Message::info("borrowed");
        sink.write(&msg).unwrap();
        assert!(!sink.writer().is_empty());
    }

    /// The rendered line of `text` from a sink that escapes for the terminal.
    ///
    /// A fresh test process starts without `-8` and without the client's
    /// `ic_chck` check, so the terminal style is upstream's `use_isprint`
    /// byte rule (log.c:499 on a server or a `-8`-less client lacking iconv).
    fn terminal_line(text: &'static str) -> Vec<u8> {
        let mut sink = make_sink();
        sink.set_terminal_escaping(true);
        sink.write(Message::warning(text)).unwrap();
        sink.into_inner()
    }

    fn plain_line(text: &'static str) -> Vec<u8> {
        let mut sink = make_sink();
        sink.write(Message::warning(text)).unwrap();
        sink.into_inner()
    }

    /// upstream: log.c:288-291 `filtered_char_len()` escapes C0 bytes but
    /// writes a tab raw. The rest of the line keeps the plain rendering.
    #[test]
    fn terminal_escaping_escapes_c0_but_keeps_a_tab() {
        assert_eq!(
            terminal_line("rule a\tb\u{b}c"),
            plain_line("rule a\tb\\#013c")
        );
    }

    /// upstream: log.c:290-291 - DEL and the C1 range are escaped, and under
    /// `use_isprint` so is every other byte `isprint()` rejects, byte by byte.
    #[test]
    fn terminal_escaping_escapes_del_and_high_bytes() {
        assert_eq!(
            terminal_line("x\u{7f}y\u{85}z"),
            plain_line("x\\#177y\\#302\\#205z")
        );
    }

    /// upstream: log.c:288-289 - a literal `\#ddd` in the text has its
    /// backslash escaped, so it cannot pass for an escape the filter wrote.
    #[test]
    fn terminal_escaping_guards_a_literal_escape_sequence() {
        assert_eq!(
            terminal_line("n\\#001\u{1}"),
            plain_line("n\\#134#001\\#001")
        );
    }

    /// upstream: log.c:460 peels only the trailing newline, so a newline
    /// inside the text is escaped and cannot forge a second line.
    #[test]
    fn terminal_escaping_keeps_an_embedded_newline_on_one_line() {
        let line = terminal_line("a\nb");
        assert_eq!(line, plain_line("a\\#012b"));
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
    }

    /// The trailing newline of a text rendered without the sink's own newline
    /// is the line terminator upstream writes raw (log.c:460).
    #[test]
    fn terminal_escaping_writes_a_texts_trailing_newline_raw() {
        let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        sink.set_terminal_escaping(true);
        sink.write(Message::warning("v\u{1}\n")).unwrap();
        assert!(sink.into_inner().ends_with(b"v\\#001\n"));
    }

    /// upstream: xattrs.c:1253-1258 and the like print an unprefixed cause
    /// line through `rprintf(FERROR, ...)`; log.c:499 escapes it before the
    /// newline, and a tab stays raw.
    #[test]
    fn write_escaped_line_escapes_a_pre_formatted_cause_line() {
        let mut sink = make_sink();
        sink.set_terminal_escaping(true);
        sink.write_escaped_line("link_stat \"no\u{1}p\te\" failed")
            .unwrap();
        assert_eq!(sink.into_inner(), b"link_stat \"no\\#001p\te\" failed\n");
    }
}
