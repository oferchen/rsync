use std::fmt;

use crate::line_mode::LineMode;
use rsync_core::message::MessageScratch;

/// Streaming sink that renders [`rsync_core::message::Message`] values into an
/// [`std::io::Write`] target.
///
/// The sink owns the underlying writer together with a reusable
/// [`MessageScratch`] buffer. Each call to [`write`](Self::write) renders the
/// supplied message using the configured [`LineMode`], mirroring upstream
/// rsync's line-oriented diagnostics by default. The helper keeps all state on
/// the stack, making it inexpensive to clone or move the sink when logging
/// contexts change.
///
/// # Examples
///
/// Collect diagnostics into a [`Vec<u8>`] with newline terminators:
///
/// ```
/// use rsync_core::message::Message;
/// use rsync_logging::MessageSink;
///
/// let mut sink = MessageSink::new(Vec::new());
///
/// sink.write(Message::warning("vanished"))?;
/// sink.write(Message::error(23, "partial"))?;
///
/// let output = String::from_utf8(sink.into_inner()).unwrap();
/// assert!(output.ends_with('\n'));
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Render a message without appending a newline:
///
/// ```
/// use rsync_core::message::Message;
/// use rsync_logging::{LineMode, MessageSink};
///
/// let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
/// sink.write(Message::info("ready"))?;
///
/// assert_eq!(sink.into_inner(), b"rsync info: ready".to_vec());
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Reuse an existing [`MessageScratch`] when constructing a new sink:
///
/// ```
/// use rsync_core::message::{Message, MessageScratch};
/// use rsync_logging::{LineMode, MessageSink};
///
/// let mut sink = MessageSink::with_parts(Vec::new(), MessageScratch::new(), LineMode::WithoutNewline);
/// sink.write(Message::info("phase one"))?;
/// let (writer, scratch, mode) = sink.into_parts();
/// assert_eq!(mode, LineMode::WithoutNewline);
///
/// let mut sink = MessageSink::with_parts(writer, scratch, LineMode::WithNewline);
/// sink.write(Message::warning("phase two"))?;
/// let output = String::from_utf8(sink.into_inner()).unwrap();
/// assert!(output.contains("phase two"));
/// # Ok::<(), std::io::Error>(())
/// ```
#[doc(alias = "--msgs2stderr")]
#[derive(Clone)]
pub struct MessageSink<W> {
    writer: W,
    scratch: MessageScratch,
    line_mode: LineMode,
}

mod methods;

impl<W> fmt::Debug for MessageSink<W>
where
    W: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageSink")
            .field("writer", &self.writer)
            .field("line_mode", &self.line_mode)
            .finish()
    }
}
