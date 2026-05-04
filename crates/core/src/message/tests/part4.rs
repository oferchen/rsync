#[test]
fn render_to_with_scratch_matches_standard_rendering() {
    let message = Message::warning("soft limit reached")
        .with_code(24)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut reused = String::new();
    message
        .render_to_with_scratch(&mut scratch, &mut reused)
        .expect("rendering into a string never fails");

    let mut baseline = String::new();
    message
        .render_to(&mut baseline)
        .expect("rendering into a string never fails");

    assert_eq!(reused, baseline);
}

#[test]
fn render_to_writer_matches_render_to_for_negative_codes() {
    let message = Message::error(-35, "timeout in data send")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    message
        .render_to_writer(&mut buffer)
        .expect("writing into a vector never fails");

    assert_eq!(buffer, message.to_string().into_bytes());
}

#[test]
fn segments_match_rendered_output() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, true);

    let mut aggregated = Vec::new();
    for slice in segments.as_slices() {
        aggregated.extend_from_slice(slice.as_ref());
    }

    assert_eq!(aggregated, message.to_line_bytes().unwrap());
    assert_eq!(segments.len(), aggregated.len());
    assert!(segments.segment_count() > 1);
}

#[test]
fn segments_handle_messages_without_optional_fields() {
    let message = Message::info("protocol handshake complete");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    let mut combined = Vec::new();
    for slice in segments.as_slices() {
        combined.extend_from_slice(slice.as_ref());
    }

    assert_eq!(combined, message.to_bytes().unwrap());
    assert_eq!(segments.segment_count(), segments.as_slices().len());
    assert!(!segments.is_empty());
}

#[test]
fn render_line_to_writer_appends_newline() {
    let message = Message::info("protocol handshake complete");

    let mut buffer = Vec::new();
    message
        .render_line_to_writer(&mut buffer)
        .expect("writing into a vector never fails");

    assert_eq!(buffer, format!("{message}\n").into_bytes());
}

#[test]
fn to_bytes_matches_display_output() {
    let message = Message::error(11, "read failure")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let rendered = message.to_bytes().expect("Vec<u8> writes are infallible");
    let expected = message.to_string().into_bytes();

    assert_eq!(rendered, expected);
}

#[test]
fn byte_len_matches_rendered_length() {
    let message = Message::error(35, "timeout waiting for daemon connection")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let rendered = message.to_bytes().expect("Vec<u8> writes are infallible");

    assert_eq!(message.byte_len(), rendered.len());
}

#[test]
fn to_line_bytes_appends_newline() {
    let message = Message::warning("vanished")
        .with_code(24)
        .with_source(message_source!());

    let rendered = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");
    let expected = {
        let mut buf = message.to_string().into_bytes();
        buf.push(b'\n');
        buf
    };

    assert_eq!(rendered, expected);
}

#[test]
fn line_byte_len_matches_rendered_length() {
    let message = Message::warning("some files vanished")
        .with_code(24)
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let rendered = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");

    assert_eq!(message.line_byte_len(), rendered.len());
}

#[test]
fn append_to_vec_matches_to_bytes() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    let appended = message
        .append_to_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(buffer, message.to_bytes().unwrap());
    assert_eq!(appended, buffer.len());
}

#[test]
fn append_line_to_vec_matches_to_line_bytes() {
    let message = Message::warning("vanished")
        .with_code(24)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    let appended = message
        .append_line_to_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(buffer, message.to_line_bytes().unwrap());
    assert_eq!(appended, buffer.len());
}

#[test]
fn append_with_scratch_accumulates_messages() {
    let message = Message::error(11, "read failure")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut buffer = Vec::new();
    let appended = message
        .append_to_vec_with_scratch(&mut scratch, &mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");
    let first_len = buffer.len();
    let without_newline = message.to_bytes().unwrap();
    assert_eq!(appended, without_newline.len());

    let appended_line = message
        .append_line_to_vec_with_scratch(&mut scratch, &mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");
    let with_newline = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");
    assert_eq!(appended_line, with_newline.len());

    assert_eq!(&buffer[..first_len], without_newline.as_slice());
    assert_eq!(&buffer[first_len..], with_newline.as_slice());
}

#[test]
fn to_bytes_with_scratch_matches_standard_rendering() {
    let message = Message::info("protocol handshake complete").with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let reused = message
        .to_line_bytes_with_scratch(&mut scratch)
        .expect("Vec<u8> writes are infallible");

    let baseline = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");

    assert_eq!(reused, baseline);
}

struct FailingWriter;

impl io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("sink error"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn render_to_writer_propagates_io_error() {
    let mut writer = FailingWriter;
    let message = Message::info("protocol handshake complete");

    let err = message
        .render_to_writer(&mut writer)
        .expect_err("writer error should propagate");

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(err.to_string(), "sink error");
}

struct NewlineFailingWriter;

impl io::Write for NewlineFailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf == b"\n" {
            Err(io::Error::other("newline sink error"))
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn render_line_to_writer_propagates_newline_error() {
    let mut writer = NewlineFailingWriter;
    let message = Message::warning("soft limit reached");

    let err = message
        .render_line_to_writer(&mut writer)
        .expect_err("newline error should propagate");

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(err.to_string(), "newline sink error");
}

#[derive(Default)]
struct InterruptingVectoredWriter {
    buffer: Vec<u8>,
    remaining_interrupts: usize,
}

impl InterruptingVectoredWriter {
    fn new(interruptions: usize) -> Self {
        Self {
            remaining_interrupts: interruptions,
            ..Self::default()
        }
    }
}

impl io::Write for InterruptingVectoredWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        if self.remaining_interrupts > 0 {
            self.remaining_interrupts -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }

        let mut written = 0usize;
        for slice in bufs {
            self.buffer.extend_from_slice(slice.as_ref());
            written += slice.len();
        }

        Ok(written)
    }
}

#[test]
fn render_to_writer_retries_after_interrupted_vectored_write() {
    let message = Message::info("protocol negotiation complete");
    let mut writer = InterruptingVectoredWriter::new(1);

    message
        .render_to_writer(&mut writer)
        .expect("interrupted writes should be retried");

    assert_eq!(writer.remaining_interrupts, 0);
    assert_eq!(writer.buffer, message.to_string().into_bytes());
}

#[test]
fn render_to_writer_uses_thread_local_scratch_per_thread() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    let message = Message::error(42, "per-thread scratch")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let barrier = Arc::new(Barrier::new(4));

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let message = message.clone();

            thread::spawn(move || {
                barrier.wait();
                let expected = message.to_string().into_bytes();

                for _ in 0..64 {
                    let mut buffer = Vec::new();
                    message
                        .render_to_writer(&mut buffer)
                        .expect("Vec<u8> writes are infallible");

                    assert_eq!(buffer, expected);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread panicked");
    }
}

#[test]
fn render_to_writer_coalesces_segments_for_vectored_writer() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(untracked_source());

    let expected = message.to_string();

    let mut writer = RecordingWriter::new();
    message
        .render_to_writer(&mut writer)
        .expect("vectored write succeeds");

    assert_eq!(writer.vectored_calls, 1, "single vectored write expected");
    assert_eq!(
        writer.write_calls, 0,
        "sequential fallback should be unused"
    );
    assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
}

#[test]
fn render_to_writer_skips_vectored_when_writer_does_not_support_it() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Receiver)
        .with_source(untracked_source());

    let expected = message.to_string();

    let mut writer = RecordingWriter::without_vectored();
    message
        .render_to_writer(&mut writer)
        .expect("sequential write succeeds");

    assert_eq!(writer.vectored_calls, 0, "vectored writes must be skipped");
    assert!(
        writer.write_calls > 0,
        "sequential path should handle the message"
    );
    assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
}

#[test]
fn render_to_writer_falls_back_when_vectored_partial() {
    let message = Message::error(30, "timeout in data send/receive")
        .with_role(Role::Receiver)
        .with_source(untracked_source());

    let expected = message.to_string();

    let mut writer = RecordingWriter::with_vectored_limit(5);
    message
        .render_to_writer(&mut writer)
        .expect("fallback write succeeds");

    assert!(
        writer.vectored_calls >= 1,
        "vectored path should be attempted at least once"
    );
    assert!(
        writer.write_calls > 0,
        "sequential fallback must finish the message"
    );
    assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
}

#[test]
fn segments_as_ref_exposes_slice_view() {
    let mut scratch = MessageScratch::new();
    let message = Message::error(35, "timeout waiting for daemon connection")
        .with_role(Role::Sender)
        .with_source(untracked_source());

    let segments = message.as_segments(&mut scratch, false);
    let slices = segments.as_ref();

    assert_eq!(slices.len(), segments.segment_count());

    let flattened: Vec<u8> = slices
        .iter()
        .flat_map(|slice| {
            let bytes: &[u8] = slice.as_ref();
            bytes.iter().copied()
        })
        .collect();

    assert_eq!(flattened, message.to_bytes().unwrap());
}

#[test]
fn segments_into_iter_collects_bytes() {
    let mut scratch = MessageScratch::new();
    let message = Message::warning("some files vanished")
        .with_code(24)
        .with_source(untracked_source());

    let segments = message.as_segments(&mut scratch, true);
    let mut flattened = Vec::new();

    for slice in segments.clone() {
        let bytes: &[u8] = slice.as_ref();
        flattened.extend_from_slice(bytes);
    }

    assert_eq!(flattened, message.to_line_bytes().unwrap());
}

