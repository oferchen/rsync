use super::*;
use std::collections::VecDeque;
use std::io::{self, IoSlice, Write};
use std::slice;

#[test]
fn send_msg_prefers_vectored_writes_when_supported() {
    struct RecordingWriter {
        writes: Vec<u8>,
        write_calls: usize,
        vectored_calls: usize,
    }

    impl RecordingWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                write_calls: 0,
                vectored_calls: 0,
            }
        }
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_calls += 1;
            let mut written = 0;
            for buf in bufs {
                self.writes.extend_from_slice(buf);
                written += buf.len();
            }
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = RecordingWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Warning, payload).expect("send succeeds");

    assert_eq!(writer.write_calls, 0, "fallback write() should not be used");
    assert_eq!(writer.vectored_calls, 1, "single vectored call expected");

    let header = MessageHeader::new(MessageCode::Warning, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_msg_falls_back_when_vectored_is_not_supported() {
    struct NoVectoredWriter {
        writes: Vec<u8>,
        write_calls: usize,
        vectored_attempts: usize,
    }

    impl NoVectoredWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                write_calls: 0,
                vectored_attempts: 0,
            }
        }
    }

    impl Write for NoVectoredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_attempts += 1;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored IO disabled",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = NoVectoredWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Warning, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Warning, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);

    assert_eq!(writer.vectored_attempts, 1);
    assert_eq!(writer.write_calls, 2);
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_msg_falls_back_after_vectored_reports_unsupported() {
    struct UnsupportedVectoredWriter {
        writes: Vec<u8>,
        vectored_attempts: usize,
        sequential_calls: usize,
    }

    impl UnsupportedVectoredWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                vectored_attempts: 0,
                sequential_calls: 0,
            }
        }
    }

    impl Write for UnsupportedVectoredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.sequential_calls += 1;
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_attempts += 1;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored IO disabled",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = UnsupportedVectoredWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Info, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);

    assert_eq!(
        writer.vectored_attempts, 1,
        "one vectored attempt should occur before fallback"
    );
    assert!(
        writer.sequential_calls >= 2,
        "fallback must write header and payload sequentially"
    );
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_msg_handles_partial_vectored_writes() {
    struct PartialWriter {
        schedule: VecDeque<usize>,
        written: Vec<u8>,
        write_calls: usize,
    }

    impl PartialWriter {
        fn new(schedule: VecDeque<usize>) -> Self {
            Self {
                schedule,
                written: Vec::new(),
                write_calls: 0,
            }
        }

        fn record(&mut self, mut remaining: usize, bufs: &[IoSlice<'_>]) -> usize {
            let mut produced = 0usize;

            for buf in bufs {
                if remaining == 0 {
                    break;
                }
                if buf.is_empty() {
                    continue;
                }

                let take = buf.len().min(remaining);
                self.written.extend_from_slice(&buf[..take]);
                produced += take;
                remaining -= take;

                if take < buf.len() {
                    break;
                }
            }

            produced
        }
    }

    impl Write for PartialWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_vectored(slice::from_ref(&IoSlice::new(buf)))
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.write_calls += 1;

            let allowed = self.schedule.pop_front().unwrap_or(usize::MAX);
            debug_assert!(
                allowed != 0,
                "partial writer schedule must contain positive chunk sizes",
            );
            if allowed == 0 {
                return Ok(0);
            }

            let produced = self.record(allowed, bufs);
            if produced == 0 {
                return Ok(0);
            }

            Ok(produced)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = PartialWriter::new(VecDeque::from(vec![2, 1, 3, 2]));
    let payload = b"chunked-payload";
    send_msg(&mut writer, MessageCode::Info, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);

    assert_eq!(writer.written, expected);
    assert!(
        writer.write_calls >= 4,
        "partial schedule should trigger repeated writes"
    );
}

#[test]
fn send_msg_retries_on_interrupted_vectored_writes() {
    struct InterruptOnceWriter {
        writes: Vec<u8>,
        vectored_attempts: usize,
        vectored_successes: usize,
        interrupted: bool,
    }

    impl InterruptOnceWriter {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                vectored_attempts: 0,
                vectored_successes: 0,
                interrupted: false,
            }
        }
    }

    impl Write for InterruptOnceWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_attempts += 1;
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "simulated EINTR",
                ));
            }

            let mut written = 0;
            for buf in bufs {
                self.writes.extend_from_slice(buf);
                written += buf.len();
            }
            self.vectored_successes += 1;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = InterruptOnceWriter::new();
    let payload = b"payload";
    send_msg(&mut writer, MessageCode::Info, payload).expect("retry succeeds after EINTR");

    assert!(writer.interrupted, "writer should have seen an interrupt");
    assert_eq!(writer.vectored_attempts, 2, "exactly one retry expected");
    assert_eq!(
        writer.vectored_successes, 1,
        "second attempt should succeed"
    );

    let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);
    assert_eq!(writer.writes, expected);
}

#[test]
fn send_msg_vectored_handles_partial_writes() {
    struct ChunkedWriter {
        max_chunk: usize,
        data: Vec<u8>,
        calls: usize,
    }

    impl ChunkedWriter {
        fn new(max_chunk: usize) -> Self {
            Self {
                max_chunk,
                data: Vec::new(),
                calls: 0,
            }
        }
    }

    impl Write for ChunkedWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            panic!("send_msg should rely on vectored writes when available");
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.calls += 1;
            let mut remaining = self.max_chunk;
            let mut written = 0;

            for buf in bufs {
                if buf.is_empty() || remaining == 0 {
                    continue;
                }

                let take = remaining.min(buf.len());
                self.data.extend_from_slice(&buf[..take]);
                remaining -= take;
                written += take;

                if take < buf.len() {
                    break;
                }
            }

            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = ChunkedWriter::new(2);
    let payload = b"abcdefgh";
    send_msg(&mut writer, MessageCode::Client, payload).expect("send succeeds");

    let header = MessageHeader::new(MessageCode::Client, payload.len() as u32).unwrap();
    let mut expected = Vec::from(header.encode());
    expected.extend_from_slice(payload);
    assert_eq!(writer.data, expected);
    assert!(
        writer.calls > 1,
        "partial writes should require multiple calls"
    );
}
