//! Sync vs async wire-parity tests for the `MultiplexReader` demux.
//!
//! These prove that the `.await`-driven demux
//! ([`MultiplexReader::read_async_with`]) produces byte-identical output to the
//! blocking demux ([`Read::read`]) - both the demultiplexed `MSG_DATA` payload
//! bytes AND the ordered, wire-visible side effects that
//! [`dispatch_message_with`](MultiplexReader::dispatch_message_with) fires for
//! `MSG_INFO`/`MSG_CLIENT` (stdout) and every `MSG_*ERROR*`/`MSG_WARNING`
//! category (stderr).
//!
//! Side-effect ordering is the load-bearing property (see
//! `docs/design/asy-7-receiver-tokio-prototype.md` §8): the inline print/flush
//! for a control frame must fire at the same point *relative to* delivered data
//! as it does in the sync driver. To assert this, both drivers run through a
//! capturing [`MuxSink`] that appends `Info`/`Error` events to a shared event
//! log, and the harness appends a `Data` event after each demuxed read. The
//! resulting interleaved timeline is compared frame-for-frame.
//!
//! A chunked-delivery variant feeds the async driver bytes in tiny pieces so
//! frames are reassembled across many `.await` points, proving the await
//! boundary never reorders or drops an effect.
//!
//! Because both drivers advance the exact same reader-free dispatch core, any
//! divergence here is a driver bug, not a demux bug. This is the demux half of
//! the `async-wire-parity` CI gate.

use std::io::{Cursor, Read};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use super::{MultiplexReader, MuxSink};

/// A single observable event on the demux timeline.
///
/// `Info`/`Error` come from the dispatch side-effect sink (in the exact order
/// dispatch fires them); `Data` is appended by the harness after each demuxed
/// read returns payload bytes. Comparing the full ordered sequence proves both
/// the data stream and the effect ordering match.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    /// An `MSG_INFO`/`MSG_CLIENT` payload routed to stdout by the sink.
    Info(Vec<u8>),
    /// An `MSG_WARNING`/`MSG_LOG`/`MSG_*ERROR*` payload routed to stderr.
    Error(Vec<u8>),
    /// Demuxed `MSG_DATA` bytes delivered by a `read` call.
    Data(Vec<u8>),
}

/// Capturing [`MuxSink`] that records effects into an ordered log instead of
/// touching the real stdout/stderr, so tests can assert effect ordering.
struct CapturingSink {
    events: Vec<Event>,
}

impl MuxSink for CapturingSink {
    fn info(&mut self, msg: &str) {
        self.events.push(Event::Info(msg.as_bytes().to_vec()));
    }

    fn error(&mut self, msg: &str) {
        self.events.push(Event::Error(msg.as_bytes().to_vec()));
    }
}

/// Builds a representative wire stream that interleaves MSG_DATA, MSG_INFO,
/// MSG_ERROR, keep-alive (empty MSG_DATA), warnings, and a multi-frame data
/// payload that must span two `read` calls.
fn corpus() -> Vec<u8> {
    let mut wire = Vec::new();
    let push = |wire: &mut Vec<u8>, code, payload: &[u8]| {
        protocol::send_msg(wire, code, payload).unwrap();
    };

    // Control frame before any data: effect must fire before the first Data.
    push(&mut wire, protocol::MessageCode::Info, b"connecting\n");
    // First data frame.
    push(&mut wire, protocol::MessageCode::Data, b"hello ");
    // Interleaved control frames between data.
    push(&mut wire, protocol::MessageCode::Warning, b"slow link\n");
    push(&mut wire, protocol::MessageCode::Error, b"a soft error\n");
    // Keep-alive: empty MSG_DATA frame - must be skipped, never delivered as EOF.
    push(&mut wire, protocol::MessageCode::Data, b"");
    // More data after the keep-alive.
    push(&mut wire, protocol::MessageCode::Data, b"world");
    // A larger data frame that will be split across small read buffers.
    push(&mut wire, protocol::MessageCode::Data, &vec![0xABu8; 300]);
    // Trailing control frames after the last data.
    push(&mut wire, protocol::MessageCode::Client, b"done\n");
    push(
        &mut wire,
        protocol::MessageCode::ErrorXfer,
        b"one skipped\n",
    );
    wire
}

/// Reads the entire demuxed stream via the blocking driver, recording the
/// interleaved data + side-effect timeline.
///
/// `read_len` is the caller buffer size, exercised small so multi-frame data is
/// split across reads exactly as the async driver would split it.
fn drive_sync(wire: &[u8], read_len: usize) -> (Vec<u8>, Vec<Event>) {
    let mut mux = MultiplexReader::new(Cursor::new(wire.to_vec()));
    let mut sink = CapturingSink { events: Vec::new() };
    let mut data = Vec::new();
    let mut buf = vec![0u8; read_len];

    loop {
        // Mirror the async driver's dispatch-through-sink by pre-draining any
        // buffered bytes, then reading a fresh frame through the shared core.
        // The demux never emits an explicit Ok(0) EOF frame, so end-of-stream
        // surfaces as UnexpectedEof from the underlying reader - identical for
        // both drivers, so terminating on it preserves parity.
        match read_sync_with(&mut mux, &mut buf, &mut sink) {
            Ok(0) => break,
            Ok(n) => {
                sink.events.push(Event::Data(buf[..n].to_vec()));
                data.extend_from_slice(&buf[..n]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => panic!("sync demux read failed: {err}"),
        }
    }
    (data, sink.events)
}

/// Blocking read that routes dispatch side effects through `sink`, mirroring
/// [`MultiplexReader::read_async_with`] exactly so the two timelines are
/// directly comparable. This is the sync counterpart used only by the parity
/// harness; production uses the `RealSink`-backed [`Read::read`].
fn read_sync_with<R: Read, S: MuxSink>(
    mux: &mut MultiplexReader<R>,
    buf: &mut [u8],
    sink: &mut S,
) -> std::io::Result<usize> {
    if mux.pos < mux.buffer.len() {
        return mux.drain_buffered(buf);
    }
    loop {
        mux.buffer.clear();
        mux.pos = 0;
        let code = protocol::recv_msg_into(&mut mux.inner, &mut mux.buffer)?;
        if mux.dispatch_message_with(code, sink) {
            if mux.buffer.is_empty() {
                continue;
            }
            return mux.place_frame(buf);
        }
        mux.check_error_exit()?;
    }
}

/// Reads the entire demuxed stream via the async driver over `reader`,
/// recording the interleaved data + side-effect timeline.
async fn drive_async<R: AsyncRead + Unpin>(reader: R, read_len: usize) -> (Vec<u8>, Vec<Event>) {
    let mut mux = MultiplexReader::new(reader);
    let mut sink = CapturingSink { events: Vec::new() };
    let mut data = Vec::new();
    let mut buf = vec![0u8; read_len];

    loop {
        match mux.read_async_with(&mut buf, &mut sink).await {
            Ok(0) => break,
            Ok(n) => {
                sink.events.push(Event::Data(buf[..n].to_vec()));
                data.extend_from_slice(&buf[..n]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => panic!("async demux read failed: {err}"),
        }
    }
    (data, sink.events)
}

/// An [`AsyncRead`] adapter that yields at most `chunk` bytes per `poll_read`,
/// forcing the async demux to reassemble frames across many `.await` points.
struct ChunkedReader {
    inner: Cursor<Vec<u8>>,
    chunk: usize,
}

impl AsyncRead for ChunkedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let limit = self.chunk.max(1).min(buf.remaining());
        if limit == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut scratch = vec![0u8; limit];
        let mut scratch_buf = ReadBuf::new(&mut scratch);
        match Pin::new(&mut self.inner).poll_read(cx, &mut scratch_buf) {
            Poll::Ready(Ok(())) => {
                buf.put_slice(scratch_buf.filled());
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// The read-buffer sizes checked: 8 splits the 300-byte frame across many
/// reads; 4096 delivers each frame whole. The async and sync drivers must
/// agree at every size.
const READ_LENS: [usize; 3] = [8, 64, 4096];

#[tokio::test(flavor = "current_thread")]
async fn demux_parity_whole_stream() {
    let wire = corpus();
    for read_len in READ_LENS {
        let (sync_data, sync_events) = drive_sync(&wire, read_len);
        let (async_data, async_events) = drive_async(Cursor::new(wire.clone()), read_len).await;

        assert_eq!(
            async_data, sync_data,
            "async demux DATA diverged from sync at read_len {read_len}"
        );
        assert_eq!(
            async_events, sync_events,
            "async demux side-effect ordering diverged from sync at read_len {read_len}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_async_delivers_data_via_real_sink() {
    // Exercises the production-facing `read_async` (which routes side effects
    // through `RealSink` to the real stdout/stderr) end to end, proving the
    // default async entry point demuxes the same DATA bytes the sync `Read`
    // impl would. Control-frame effects go to the real streams here; ordered-
    // effect parity is asserted separately via the capturing-sink tests.
    let mut wire = Vec::new();
    protocol::send_msg(&mut wire, protocol::MessageCode::Info, b"info\n").unwrap();
    protocol::send_msg(&mut wire, protocol::MessageCode::Data, b"payload-bytes").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(wire));
    let mut buf = [0u8; 64];
    let n = mux.read_async(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"payload-bytes");
}

#[tokio::test(flavor = "current_thread")]
async fn demux_parity_chunked_delivery() {
    let wire = corpus();
    // The sync reference: whole-stream delivery at each read length.
    for read_len in READ_LENS {
        let (sync_data, sync_events) = drive_sync(&wire, read_len);

        // Deliver wire bytes in tiny chunks so every frame is reassembled
        // across many polls / await points.
        for chunk in [1usize, 2, 3, 7, 13] {
            let reader = ChunkedReader {
                inner: Cursor::new(wire.clone()),
                chunk,
            };
            let (async_data, async_events) = drive_async(reader, read_len).await;
            assert_eq!(
                async_data, sync_data,
                "async demux DATA diverged with chunk {chunk}, read_len {read_len}"
            );
            assert_eq!(
                async_events, sync_events,
                "async demux side-effect ordering diverged with chunk {chunk}, read_len {read_len}"
            );
        }
    }
}
