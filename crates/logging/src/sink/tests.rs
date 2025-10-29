use super::*;
use crate::LineMode;
use rsync_core::message::{Message, MessageScratch};
use std::io::{self, Cursor, Write};

#[test]
fn debug_representation_mentions_writer_and_line_mode() {
    let sink = MessageSink::with_line_mode(Vec::<u8>::new(), LineMode::WithoutNewline);
    let rendered = format!("{:?}", sink);
    assert!(rendered.starts_with("MessageSink"));
    assert!(
        rendered.contains("writer: []"),
        "debug output should expose the writer state"
    );
    assert!(
        rendered.contains("line_mode: WithoutNewline"),
        "debug output should reflect the configured line mode"
    );
}

#[derive(Default)]
struct TrackingWriter {
    buffer: Vec<u8>,
    flush_calls: usize,
}

impl Write for TrackingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_calls += 1;
        Ok(())
    }
}

struct FailingFlushWriter;

impl Write for FailingFlushWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failed"))
    }
}

#[test]
fn scratch_accessors_expose_reusable_buffer() {
    let mut sink = MessageSink::new(Vec::<u8>::new());
    let shared_ptr = {
        let scratch = sink.scratch();
        scratch as *const MessageScratch
    };
    let mutable_ptr = {
        let scratch = sink.scratch_mut();
        scratch as *mut MessageScratch
    };

    assert_eq!(shared_ptr, mutable_ptr as *const MessageScratch);

    // Reset the scratch buffer and ensure rendering still succeeds.
    *sink.scratch_mut() = MessageScratch::new();
    sink.write(Message::info("ready"))
        .expect("write succeeds after manual scratch reset");

    let rendered = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert_eq!(rendered, "rsync info: ready\n");
}

#[test]
fn writer_accessors_expose_underlying_writer() {
    let mut sink = MessageSink::new(Vec::<u8>::new());
    assert!(sink.writer().is_empty());

    sink.writer_mut().extend_from_slice(b"prefill");
    sink.write(Message::info("status")).expect("write succeeds");

    let expected = b"prefillrsync info: status\n".to_vec();
    assert_eq!(sink.writer().as_slice(), expected.as_slice());

    let rendered = sink.into_inner();
    assert_eq!(rendered, expected);
}

#[test]
fn line_mode_bool_conversions_round_trip() {
    assert_eq!(LineMode::from(true), LineMode::WithNewline);
    assert_eq!(LineMode::from(false), LineMode::WithoutNewline);

    let append: bool = LineMode::WithNewline.into();
    assert!(append);

    let append: bool = LineMode::WithoutNewline.into();
    assert!(!append);
}

#[test]
fn sink_appends_newlines_by_default() {
    let mut sink = MessageSink::new(Vec::new());
    sink.write(Message::warning("vanished"))
        .expect("write succeeds");
    sink.write(Message::error(23, "partial"))
        .expect("write succeeds");

    let output = String::from_utf8(sink.into_inner()).expect("utf-8");
    let mut lines = output.lines();
    assert_eq!(lines.next(), Some("rsync warning: vanished"));
    assert_eq!(lines.next(), Some("rsync error: partial (code 23)"));
    assert!(lines.next().is_none());
}

#[test]
fn sink_without_newline_preserves_output() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    sink.write(Message::info("ready")).expect("write succeeds");

    let output = sink.into_inner();
    assert_eq!(output, b"rsync info: ready".to_vec());
}

#[test]
fn write_accepts_owned_messages() {
    let mut sink = MessageSink::new(Vec::new());
    sink.write(Message::info("phase one"))
        .expect("owned message write succeeds");
    sink.write(Message::warning("phase two"))
        .expect("owned message write succeeds");

    let rendered = String::from_utf8(sink.into_inner()).expect("utf-8");
    let mut lines = rendered.lines();
    assert_eq!(lines.next(), Some("rsync info: phase one"));
    assert_eq!(lines.next(), Some("rsync warning: phase two"));
    assert!(lines.next().is_none());
}

#[test]
fn map_writer_preserves_configuration() {
    use std::io::Cursor;

    let sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    let mut sink = sink.map_writer(Cursor::new);
    assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

    sink.write(Message::info("ready")).expect("write succeeds");

    let cursor = sink.into_inner();
    assert_eq!(cursor.into_inner(), b"rsync info: ready".to_vec());
}

#[test]
fn try_map_writer_transforms_writer() {
    use std::io::Cursor;

    let sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    let mut sink = sink
        .try_map_writer(
            |writer| -> Result<Cursor<Vec<u8>>, (Vec<u8>, &'static str)> {
                Ok(Cursor::new(writer))
            },
        )
        .expect("mapping succeeds");
    assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

    sink.write(Message::info("ready")).expect("write succeeds");

    let cursor = sink.into_inner();
    assert_eq!(cursor.into_inner(), b"rsync info: ready".to_vec());
}

#[test]
fn replace_writer_swaps_underlying_writer() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    sink.write(Message::info("phase one"))
        .expect("write succeeds");

    let previous = sink.replace_writer(Vec::new());
    assert_eq!(previous, b"rsync info: phase one".to_vec());

    sink.write(Message::info("phase two"))
        .expect("write succeeds");
    assert_eq!(sink.into_inner(), b"rsync info: phase two".to_vec());
}

#[test]
fn try_map_writer_preserves_sink_on_error() {
    let sink = MessageSink::new(Vec::new());
    let err = sink
        .try_map_writer(|writer| -> Result<Vec<u8>, (Vec<u8>, &'static str)> {
            Err((writer, "conversion failed"))
        })
        .unwrap_err();
    let (mut sink, error) = err.into_parts();

    assert_eq!(error, "conversion failed");

    sink.write(Message::info("still running"))
        .expect("write succeeds");

    let output = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert_eq!(output, "rsync info: still running\n");
}

#[test]
fn try_map_writer_error_clone_preserves_state() {
    let mut original =
        TryMapWriterError::new(MessageSink::new(Vec::<u8>::new()), String::from("failure"));
    let mut cloned = original.clone();

    original
        .sink_mut()
        .write(Message::info("original"))
        .expect("write succeeds");
    cloned
        .sink_mut()
        .write(Message::info("clone"))
        .expect("write succeeds");

    assert_eq!(original.error(), "failure");
    assert_eq!(cloned.error(), "failure");

    let (original_sink, original_error) = original.into_parts();
    let (cloned_sink, cloned_error) = cloned.into_parts();

    assert_eq!(original_error, "failure");
    assert_eq!(cloned_error, "failure");

    let original_rendered = String::from_utf8(original_sink.into_inner()).expect("utf-8");
    let cloned_rendered = String::from_utf8(cloned_sink.into_inner()).expect("utf-8");

    assert!(original_rendered.contains("original"));
    assert!(cloned_rendered.contains("clone"));
}

#[test]
fn try_map_writer_error_as_ref_and_as_mut_provide_access() {
    let mut err =
        TryMapWriterError::new(MessageSink::new(Vec::<u8>::new()), String::from("failure"));
    let (sink_ref, error_ref) = err.as_ref();
    assert_eq!(sink_ref.line_mode(), LineMode::WithNewline);
    assert_eq!(error_ref, "failure");

    {
        let (sink_mut, error_mut) = err.as_mut();
        sink_mut.set_line_mode(LineMode::WithoutNewline);
        error_mut.push('!');
    }

    assert_eq!(err.sink().line_mode(), LineMode::WithoutNewline);
    assert_eq!(err.error(), "failure!");
}

#[test]
fn try_map_writer_error_map_helpers_transform_components() {
    let err = TryMapWriterError::new(MessageSink::new(Vec::<u8>::new()), String::from("failure"));

    let mapped_sink = err.clone().map_sink(|mut sink| {
        sink.set_line_mode(LineMode::WithoutNewline);
        sink
    });
    assert_eq!(mapped_sink.sink().line_mode(), LineMode::WithoutNewline);
    assert_eq!(mapped_sink.error(), "failure");

    let mapped_error = err.clone().map_error(|error| error.len());
    assert_eq!(*mapped_error.error(), "failure".len());
    assert_eq!(mapped_error.sink().line_mode(), LineMode::WithNewline);

    let mut mapped_parts = err.map_parts(|sink, error| {
        let sink = sink.map_writer(Cursor::new);
        let len = error.len();
        (sink, len)
    });
    assert_eq!(*mapped_parts.error(), "failure".len());

    mapped_parts
        .sink_mut()
        .write(Message::info("mapped"))
        .expect("write succeeds");

    let cursor = mapped_parts.into_sink().into_inner();
    let rendered = String::from_utf8(cursor.into_inner()).expect("utf-8");
    assert!(rendered.contains("mapped"));
}

#[test]
fn write_with_mode_overrides_line_mode_for_single_message() {
    let mut sink = MessageSink::new(Vec::new());
    sink.write(Message::info("phase one"))
        .expect("write succeeds");
    sink.write_with_mode(Message::info("progress"), LineMode::WithoutNewline)
        .expect("write succeeds");
    sink.write(Message::info("phase two"))
        .expect("write succeeds");

    assert_eq!(sink.line_mode(), LineMode::WithNewline);

    let output = sink.into_inner();
    let rendered = String::from_utf8(output).expect("utf-8");
    let mut lines = rendered.lines();
    assert_eq!(lines.next(), Some("rsync info: phase one"));
    assert_eq!(
        lines.next(),
        Some("rsync info: progressrsync info: phase two"),
    );
    assert!(lines.next().is_none());
}

#[test]
fn write_with_mode_respects_explicit_newline() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    sink.write_with_mode(Message::warning("vanished"), LineMode::WithNewline)
        .expect("write succeeds");

    assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

    let buffer = sink.into_inner();
    let rendered = String::from_utf8(buffer).expect("utf-8");
    assert_eq!(rendered, "rsync warning: vanished\n");
}

#[test]
fn write_with_mode_accepts_owned_messages() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    sink.write_with_mode(Message::info("phase one"), LineMode::WithNewline)
        .expect("owned message write succeeds");
    sink.write_with_mode(Message::info("phase two"), LineMode::WithoutNewline)
        .expect("owned message write succeeds");

    assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

    let buffer = sink.into_inner();
    let rendered = String::from_utf8(buffer).expect("utf-8");
    assert_eq!(rendered, "rsync info: phase one\nrsync info: phase two");
}

#[test]
fn write_segments_respects_sink_line_mode() {
    let message = Message::info("phase complete");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    let mut sink = MessageSink::new(Vec::new());
    sink.write_segments(&segments, false)
        .expect("writing segments succeeds");

    let rendered = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert_eq!(rendered, "rsync info: phase complete\n");
}

#[test]
fn write_segments_with_mode_overrides_line_mode() {
    let message = Message::info("phase complete");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    let mut sink = MessageSink::new(Vec::new());
    sink.write_segments_with_mode(&segments, LineMode::WithoutNewline, false)
        .expect("writing segments succeeds");

    let output = sink.into_inner();
    assert_eq!(output, b"rsync info: phase complete".to_vec());
}

#[test]
fn write_segments_avoids_double_newline_when_flag_set() {
    let message = Message::info("phase complete");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, true);

    let mut sink = MessageSink::new(Vec::new());
    sink.write_segments(&segments, true)
        .expect("writing segments succeeds");

    let rendered = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert_eq!(rendered, "rsync info: phase complete\n");
}

#[test]
fn write_all_streams_every_message() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithNewline);
    let messages = [
        Message::info("phase 1"),
        Message::warning("transient"),
        Message::error(10, "socket"),
    ];
    let expected = messages.len();
    sink.write_all(messages).expect("batch write succeeds");

    let output = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert_eq!(output.lines().count(), expected);
}

#[test]
fn write_all_accepts_owned_messages() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithNewline);
    let messages = vec![
        Message::info("phase 1"),
        Message::warning("transient"),
        Message::error(10, "socket"),
    ];
    let expected = messages.len();

    sink.write_all(messages).expect("batch write succeeds");

    let output = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert_eq!(output.lines().count(), expected);
}

#[test]
fn write_all_with_mode_uses_explicit_line_mode() {
    let mut sink = MessageSink::new(Vec::new());
    let progress = [Message::info("p1"), Message::info("p2")];

    sink.write_all_with_mode(progress.iter(), LineMode::WithoutNewline)
        .expect("batch write succeeds");

    assert_eq!(sink.line_mode(), LineMode::WithNewline);

    let output = sink.into_inner();
    assert_eq!(output, b"rsync info: p1rsync info: p2".to_vec());
}

#[test]
fn write_all_with_mode_accepts_owned_messages() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithNewline);
    let messages = vec![Message::info("one"), Message::info("two")];

    sink.write_all_with_mode(messages, LineMode::WithoutNewline)
        .expect("batch write succeeds");

    assert_eq!(sink.line_mode(), LineMode::WithNewline);

    let output = sink.into_inner();
    assert_eq!(output, b"rsync info: onersync info: two".to_vec());
}

#[test]
fn into_parts_allows_reusing_scratch() {
    let mut sink =
        MessageSink::with_parts(Vec::new(), MessageScratch::new(), LineMode::WithoutNewline);
    sink.write(Message::info("first")).expect("write succeeds");

    let (writer, scratch, mode) = sink.into_parts();
    assert_eq!(mode, LineMode::WithoutNewline);

    let mut sink = MessageSink::with_parts(writer, scratch, LineMode::WithNewline);
    sink.write(Message::warning("second"))
        .expect("write succeeds");

    let output = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert!(output.starts_with("rsync info: first"));
    assert!(output.contains("rsync warning: second"));
    assert!(output.ends_with('\n'));
}

#[test]
fn set_line_mode_updates_behavior() {
    let mut sink = MessageSink::new(Vec::new());
    assert_eq!(sink.line_mode(), LineMode::WithNewline);

    sink.set_line_mode(LineMode::WithoutNewline);
    assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

    sink.write(Message::info("ready")).expect("write succeeds");

    let buffer = sink.into_inner();
    assert_eq!(buffer, b"rsync info: ready".to_vec());
}

#[test]
fn scoped_line_mode_restores_previous_configuration() {
    let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    {
        let mut guard = sink.scoped_line_mode(LineMode::WithNewline);
        assert_eq!(guard.previous_line_mode(), LineMode::WithoutNewline);
        guard
            .write(Message::info("transient"))
            .expect("write succeeds");
    }

    assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    sink.write(Message::info("steady")).expect("write succeeds");

    let output = String::from_utf8(sink.into_inner()).expect("utf-8");
    assert_eq!(output, "rsync info: transient\nrsync info: steady");
}

#[test]
fn scoped_line_mode_controls_rendering_within_scope() {
    let mut sink = MessageSink::new(Vec::new());
    {
        let mut guard = sink.scoped_line_mode(LineMode::WithoutNewline);
        guard
            .write(Message::info("phase one"))
            .expect("write succeeds");
        guard
            .write(Message::info("phase two"))
            .expect("write succeeds");
    }

    sink.write(Message::info("done")).expect("write succeeds");

    let output = sink.into_inner();
    assert_eq!(
        output,
        b"rsync info: phase onersync info: phase tworsync info: done\n".to_vec()
    );
}

#[test]
fn scoped_line_mode_into_inner_keeps_override() {
    let mut sink = MessageSink::new(Vec::new());
    {
        let sink = sink.scoped_line_mode(LineMode::WithoutNewline).into_inner();
        sink.write(Message::info("phase one"))
            .expect("write succeeds");
    }

    assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    sink.write(Message::info("phase two"))
        .expect("write succeeds");

    let output = sink.into_inner();
    assert_eq!(
        output,
        b"rsync info: phase onersync info: phase two".to_vec()
    );
}

#[test]
fn flush_delegates_to_inner_writer() {
    let writer = TrackingWriter::default();
    let mut sink = MessageSink::with_line_mode(writer, LineMode::WithNewline);

    sink.flush().expect("flush succeeds");

    let writer = sink.into_inner();
    assert_eq!(writer.flush_calls, 1);
    assert!(writer.buffer.is_empty());
}

#[test]
fn flush_propagates_writer_errors() {
    let mut sink = MessageSink::with_line_mode(FailingFlushWriter, LineMode::WithNewline);

    let err = sink.flush().expect_err("flush should propagate error");
    assert_eq!(err.kind(), io::ErrorKind::Other);
}
