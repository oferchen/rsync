use std::fmt;

use crate::line_mode::LineMode;
use core::{branding::Brand, message::MessageScratch};

/// Streaming sink that renders [`core::message::Message`] values into an
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
/// use core::message::Message;
/// use logging::MessageSink;
///
/// let mut sink = MessageSink::new(Vec::new());
///
/// sink.write(Message::warning("vanished")).expect("write warning");
/// sink
///     .write(Message::error(23, "partial"))
///     .expect("write error");
///
/// let output = String::from_utf8(sink.into_inner()).unwrap();
/// assert!(output.ends_with('\n'));
/// ```
///
/// Render a message without appending a newline:
///
/// ```
/// use core::message::Message;
/// use logging::{LineMode, MessageSink};
///
/// let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
/// sink
///     .write(Message::info("ready"))
///     .expect("write info");
///
/// assert_eq!(sink.into_inner(), b"rsync info: ready".to_vec());
/// ```
///
/// Reuse an existing [`MessageScratch`] when constructing a new sink:
///
/// ```
/// use core::message::{Message, MessageScratch};
/// use logging::{LineMode, MessageSink};
///
/// let mut sink = MessageSink::with_parts(Vec::new(), MessageScratch::new(), LineMode::WithoutNewline);
/// sink
///     .write(Message::info("phase one"))
///     .expect("write phase one");
/// let (writer, scratch, mode, brand) = sink.into_parts();
/// assert_eq!(brand, core::branding::Brand::Upstream);
/// assert_eq!(mode, LineMode::WithoutNewline);
///
/// let mut sink = MessageSink::with_parts(writer, scratch, LineMode::WithNewline);
/// sink
///     .write(Message::warning("phase two"))
///     .expect("write phase two");
/// let output = String::from_utf8(sink.into_inner()).unwrap();
/// assert!(output.contains("phase two"));
/// ```
#[doc(alias = "--msgs2stderr")]
#[derive(Clone)]
pub struct MessageSink<W> {
    writer: W,
    scratch: MessageScratch,
    line_mode: LineMode,
    brand: Brand,
}

mod accessors;
mod constructors;
mod mapping;
mod writing;

impl<W> fmt::Debug for MessageSink<W>
where
    W: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageSink")
            .field("writer", &self.writer)
            .field("line_mode", &self.line_mode)
            .field("brand", &self.brand)
            .finish()
    }
}
