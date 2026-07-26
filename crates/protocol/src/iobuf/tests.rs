//! Tests for the fixed-size wire buffers.
//!
//! The invariants under test are upstream's, not ours: a reserved header that
//! is back-filled in place, a circular drain that resumes exactly where a short
//! write stopped, and buffers that never grow.

use std::io::{self, Read, Write};

use super::{IN_BUFFER_SIZE, InBuf, IoBufReader, OUT_BUFFER_SIZE, OutBuf, round_up_1024};
use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

/// Writer that accepts at most `chunk` bytes per call and records every call.
///
/// Short writes are what tear a frame that is not contiguous in one buffer, so
/// every drain test runs through one of these rather than a `Vec`.
struct ChunkedWriter {
    sink: Vec<u8>,
    chunk: usize,
    calls: Vec<usize>,
}

impl ChunkedWriter {
    fn new(chunk: usize) -> Self {
        Self {
            sink: Vec::new(),
            chunk,
            calls: Vec::new(),
        }
    }
}

impl Write for ChunkedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(self.chunk);
        self.sink.extend_from_slice(&buf[..n]);
        self.calls.push(n);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Writer that stalls with `WouldBlock` once `budget` bytes have been accepted.
///
/// Models a non-blocking descriptor filling its socket buffer at an arbitrary
/// offset - including mid-header and mid-payload.
struct StallingWriter {
    sink: Vec<u8>,
    budget: usize,
    stalled: bool,
}

impl StallingWriter {
    fn new(budget: usize) -> Self {
        Self {
            sink: Vec::new(),
            budget,
            stalled: false,
        }
    }
}

impl Write for StallingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.budget == 0 {
            self.stalled = true;
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "socket full"));
        }
        let n = buf.len().min(self.budget);
        self.sink.extend_from_slice(&buf[..n]);
        self.budget -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn data_frame(payload: &[u8]) -> Vec<u8> {
    let header = MessageHeader::new(MessageCode::Data, payload.len() as u32).unwrap();
    let mut out = Vec::from(header.encode());
    out.extend_from_slice(payload);
    out
}

/// Upstream allocates every I/O buffer in whole 1024-byte units so the low byte
/// of `size` is free to mark a temporarily reduced buffer (io.c:129-136).
#[test]
fn buffer_sizes_match_upstream() {
    assert_eq!(IN_BUFFER_SIZE, 32 * 1024, "upstream io.c:1401");
    assert_eq!(OUT_BUFFER_SIZE, 64 * 1024, "upstream io.c:1382");
    assert_eq!(round_up_1024(1), 1024);
    assert_eq!(round_up_1024(1024), 1024);
    assert_eq!(round_up_1024(1025), 2048);
    assert_eq!(OUT_BUFFER_SIZE & 0xFF, 0, "low byte must stay free");
}

/// "Empty" on a multiplexed buffer means `len == 4`, not `len == 0`: the
/// reserved header occupies the first four bytes (upstream `out_empty_len`,
/// io.c:2457). Getting this wrong flushes a bodiless header on every idle poll.
#[test]
fn reserved_header_makes_empty_mean_four() {
    let mut out = OutBuf::new(1024);
    assert!(out.is_empty());
    out.start_multiplex();

    let (pos, len, _) = out.cursors();
    assert_eq!(
        (pos, len),
        (0, HEADER_LEN),
        "header reserved, nothing written"
    );
    assert!(out.is_empty(), "a reserved header is still an empty buffer");
    assert_eq!(out.data_len(), 0);

    out.push(b"x");
    assert!(!out.is_empty());
    assert_eq!(out.data_len(), 1);
}

/// Appending bytes that fit must perform no I/O whatsoever - upstream's
/// `write_buf` only reaches `perform_io` when `out.len + len > out.size`
/// (io.c:2263-2264). `push` takes no writer at all, so this is structural; the
/// test pins that the flush that follows is a single frame, not one per push.
#[test]
fn buffered_writes_do_no_io_until_flush() {
    let mut out = OutBuf::new(1024);
    out.start_multiplex();
    for _ in 0..16 {
        out.push(b"abcd");
    }

    let mut writer = ChunkedWriter::new(usize::MAX);
    out.flush(&mut writer).unwrap();

    assert_eq!(writer.calls.len(), 1, "one write for sixteen appends");
    assert_eq!(writer.sink, data_frame(&b"abcd".repeat(16)));
    assert!(out.is_empty());
}

/// The header is back-filled inside the buffer before any syscall, so a writer
/// that accepts one byte at a time still emits the frame in order and never
/// exposes a header whose payload is not already queued behind it.
#[test]
fn short_writes_never_tear_the_frame() {
    let mut out = OutBuf::new(1024);
    out.start_multiplex();
    out.push(b"hello world");

    let mut writer = ChunkedWriter::new(1);
    out.flush(&mut writer).unwrap();

    assert_eq!(writer.sink, data_frame(b"hello world"));
    assert!(writer.calls.iter().all(|&n| n == 1), "byte-at-a-time drain");
}

/// A stall mid-header and a stall mid-payload must both leave the buffer
/// describing exactly what is still owed, so the retry resumes on the very next
/// byte. Upstream gets this from `out->pos` surviving a partial write
/// (io.c:869-877); a header written by a separate `write_all` cannot.
#[test]
fn stall_at_any_offset_resumes_without_loss() {
    let payload = b"the quick brown fox";
    let expected = data_frame(payload);

    for stall_at in 0..expected.len() {
        let mut out = OutBuf::new(1024);
        out.start_multiplex();
        out.push(payload);

        let mut writer = StallingWriter::new(stall_at);
        let err = out.flush(&mut writer).expect_err("must stall");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            writer.sink.len(),
            stall_at,
            "exactly the accepted prefix reached the wire"
        );
        assert_eq!(writer.sink, expected[..stall_at], "prefix is in order");

        // The retry drains the remainder; nothing is re-sent or dropped.
        writer.budget = usize::MAX;
        out.flush(&mut writer).unwrap();
        assert_eq!(writer.sink, expected, "resumed exactly at the stall point");
        assert!(out.is_empty());
    }
}

/// Golden bytes for a flush that wraps the circular end.
///
/// This is the case reserve-in-place is easy to get subtly wrong. A stalled
/// run leaves `pos` part-way through the buffer; the bytes appended behind it
/// split around the physical end (upstream io.c:2270-2276), the reserved header
/// for that second frame sits immediately in front of them, and the drain has
/// to stop at the end, rewind `pos` to 0 and continue (io.c:864-877). The wire
/// must still show two well-formed frames, in order, with nothing re-sent.
#[test]
fn flush_wrapping_the_circular_end_is_byte_exact() {
    // 1 KiB circular buffer: 1020 payload bytes plus the reserved header.
    let mut out = OutBuf::new(1020);
    out.start_multiplex();

    let first: Vec<u8> = (0..900u16).map(|i| i as u8).collect();
    out.push(&first);

    // Stall after 800 of the 904 frame bytes: pos = 800, 104 still owed.
    let mut writer = StallingWriter::new(800);
    assert_eq!(
        out.flush(&mut writer).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(out.cursors().0, 800, "the run resumes from here");

    // Appending now writes past the physical end and wraps to offset 0, behind
    // a reserved header that lands at 904.
    let second: Vec<u8> = (0..200u16).map(|i| (i * 7) as u8).collect();
    out.push(&second);

    writer.budget = usize::MAX;
    out.flush(&mut writer).unwrap();

    let mut expected = data_frame(&first);
    expected.extend_from_slice(&data_frame(&second));
    assert_eq!(writer.sink, expected, "golden wire bytes across the wrap");

    // A fully drained buffer rewinds and re-reserves at offset 0
    // (upstream io.c:872-877).
    assert_eq!(out.cursors(), (0, HEADER_LEN, 1024));
    assert!(out.is_empty());
}

/// A reserved header that would straddle the physical end forces upstream's
/// temporary size reduction (io.c:699-705): the buffer shrinks so the header
/// goes to offset 0 instead, and the size is restored the moment `pos` wraps
/// (io.c:497-513). The low byte of the size is the marker, which only works
/// because every allocation is 1024-aligned.
#[test]
fn header_that_would_straddle_the_end_reduces_the_size() {
    let mut out = OutBuf::new(1020);
    out.start_multiplex();

    // 1018 payload bytes put the next header slot at 1022, four bytes of which
    // will not fit before the end of the 1024-byte buffer.
    let payload: Vec<u8> = (0..1018u16).map(|i| (i * 11) as u8).collect();
    out.push(&payload);

    let mut writer = StallingWriter::new(0);
    assert_eq!(
        out.flush(&mut writer).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(out.cursors().2, 1022, "size temporarily reduced");

    writer.budget = usize::MAX;
    out.flush(&mut writer).unwrap();
    assert_eq!(writer.sink, data_frame(&payload));
    assert_eq!(out.cursors(), (0, HEADER_LEN, 1024), "size restored");

    // The buffer keeps working after the restore.
    out.push(b"after");
    out.flush(&mut writer).unwrap();
    let mut expected = data_frame(&payload);
    expected.extend_from_slice(&data_frame(b"after"));
    assert_eq!(writer.sink, expected);
}

/// A raw (non-multiplexed) buffer writes exactly the bytes pushed - no header,
/// and `out_empty_len` stays 0.
#[test]
fn raw_buffer_writes_no_header() {
    let mut out = OutBuf::new(1024);
    out.push(b"plain");
    assert_eq!(out.data_len(), 5);
    let mut writer = ChunkedWriter::new(usize::MAX);
    out.flush(&mut writer).unwrap();
    assert_eq!(writer.sink, b"plain");
    assert!(out.is_empty());
}

/// Flushing an empty buffer is a no-op: no keepalive-sized bodiless frame, no
/// syscall. upstream `io_flush` returns immediately when `out.len` is already
/// `out_empty_len`.
#[test]
fn flushing_an_empty_buffer_writes_nothing() {
    let mut out = OutBuf::new(1024);
    out.start_multiplex();
    let mut writer = ChunkedWriter::new(usize::MAX);
    out.flush(&mut writer).unwrap();
    assert!(writer.sink.is_empty());
    assert!(writer.calls.is_empty());
}

/// The input buffer is fixed for its whole lifetime (io.c:579) and wraps rather
/// than compacting.
#[test]
fn input_buffer_is_fixed_and_circular() {
    let mut buf = InBuf::new(1024);
    assert_eq!(buf.capacity(), 1024);

    let source: Vec<u8> = (0..1024u16).map(|i| i as u8).collect();
    let mut reader = &source[..];
    assert_eq!(buf.fill(&mut reader).unwrap(), 1024);
    assert_eq!(buf.len(), 1024);
    assert_eq!(
        buf.fill(&mut reader).unwrap(),
        0,
        "a full buffer never grows"
    );
    assert_eq!(buf.capacity(), 1024, "never resized");

    buf.consume(1000);
    assert_eq!(buf.len(), 24);
    assert_eq!(buf.readable(), &source[1000..]);

    // Refilling now wraps: the free span is [0, 1000).
    let more: Vec<u8> = (0..100u8).map(|i| i.wrapping_add(200)).collect();
    let mut reader = &more[..];
    assert_eq!(buf.fill(&mut reader).unwrap(), 100);
    assert_eq!(buf.len(), 124);
    assert_eq!(buf.readable(), &source[1000..], "head served first");
    buf.consume(24);
    assert_eq!(buf.readable(), &more[..], "then the wrapped tail");
    buf.consume(100);
    assert!(buf.is_empty());
}

/// Reader that yields one byte per call, so the adapter's buffering is the only
/// thing keeping syscall count sane.
struct DribbleReader<'a> {
    data: &'a [u8],
}

impl Read for DribbleReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.data.is_empty() || out.is_empty() {
            return Ok(0);
        }
        out[0] = self.data[0];
        self.data = &self.data[1..];
        Ok(1)
    }
}

/// The `Read` adapter delivers the stream unchanged regardless of how the
/// underlying descriptor fragments it.
#[test]
fn iobuf_reader_delivers_the_stream_unchanged() {
    let source: Vec<u8> = (0..5000u16).map(|i| (i * 3) as u8).collect();
    let mut reader = IoBufReader::with_capacity(1024, DribbleReader { data: &source });
    let mut got = Vec::new();
    reader.read_to_end(&mut got).unwrap();
    assert_eq!(got, source);
}

/// A request at least as large as the buffer bypasses staging, matching
/// `std::io::BufReader`; the bytes delivered must be identical either way.
#[test]
fn iobuf_reader_bypasses_for_large_requests() {
    let source: Vec<u8> = (0..4096u16).map(|i| i as u8).collect();
    let mut reader = IoBufReader::with_capacity(1024, &source[..]);
    let mut out = vec![0u8; 4096];
    let n = reader.read(&mut out).unwrap();
    assert_eq!(
        n, 4096,
        "the whole slice is served straight from the source"
    );
    assert_eq!(out, source);
}
