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
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use super::{MultiplexReader, MuxSink};
use crate::reader::{AsyncCompressedReader, AsyncServerReader, CountingReader, ServerReader};

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

/// A data-only wire stream: every frame is `MSG_DATA` so the demuxed output
/// equals the concatenated payloads. Used by the full-stack counter-parity
/// tests, where the assertion is on delivered bytes plus the raw wire byte
/// count rather than side-effect ordering (covered by the demux-only tests).
///
/// The stream ends exactly at a frame boundary and the readers drain it to EOF,
/// so both the sync `BufReader`-backed counter and the async counter observe the
/// full wire length with no buffering prefetch skew.
fn data_only_wire() -> (Vec<u8>, Vec<u8>) {
    let payloads: [&[u8]; 5] = [
        b"first-chunk",
        b"",            // keep-alive empty frame - skipped, never data
        &[0x5Au8; 200], // spans small read buffers
        b"second",
        &[0xC3u8; 40],
    ];
    let mut wire = Vec::new();
    let mut expected = Vec::new();
    for p in payloads {
        protocol::send_msg(&mut wire, protocol::MessageCode::Data, p).unwrap();
        expected.extend_from_slice(p);
    }
    (wire, expected)
}

/// Drives the full **sync** receiver reader stack over `wire`:
/// `CountingReader` (raw byte counter, below buffering) -> `io::BufReader`
/// -> `ServerReader` in multiplex mode. Returns the demuxed bytes and the final
/// raw wire byte count from the counter.
fn drive_sync_stack(wire: &[u8], read_len: usize) -> (Vec<u8>, u64) {
    let counting = CountingReader::new(Cursor::new(wire.to_vec()));
    let counter = counting.counter();
    let buffered = std::io::BufReader::with_capacity(64 * 1024, counting);
    let mut server = ServerReader::new_plain(buffered)
        .activate_multiplex()
        .expect("activate multiplex");

    let mut data = Vec::new();
    let mut buf = vec![0u8; read_len];
    loop {
        match server.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => panic!("sync stack read failed: {err}"),
        }
    }
    (data, counter.load(Ordering::Relaxed))
}

/// Drives the full **async** receiver reader stack over `reader`:
/// `CountingReader` (AsyncRead, raw byte counter) -> `tokio::io::BufReader`
/// -> `AsyncServerReader` in multiplex mode -> `MultiplexReader::read_async`.
/// Returns the demuxed bytes and the final raw wire byte count from the counter.
async fn drive_async_stack<R: AsyncRead + Unpin>(reader: R, read_len: usize) -> (Vec<u8>, u64) {
    let counting = CountingReader::new(reader);
    let counter = counting.counter();
    let buffered = tokio::io::BufReader::with_capacity(64 * 1024, counting);
    let mut server = AsyncServerReader::new_plain(buffered)
        .activate_multiplex()
        .expect("activate multiplex");
    assert!(
        server.is_multiplexed(),
        "async stack must be in multiplex mode after activation"
    );

    let mut data = Vec::new();
    let mut buf = vec![0u8; read_len];
    loop {
        match server.read_async(&mut buf).await {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => panic!("async stack read failed: {err}"),
        }
    }
    (data, counter.load(Ordering::Relaxed))
}

/// End-to-end sync-vs-async parity for the assembled receiver reader stack:
/// identical delivered bytes AND identical final `bytes_received` counts, across
/// read-buffer sizes. Proves the async twin stack
/// (`CountingReader`/`AsyncServerReader`/`read_async`) is byte-identical to the
/// sync stack and that the async counter is byte-exact.
#[tokio::test(flavor = "current_thread")]
async fn reader_stack_parity_whole_stream() {
    let (wire, expected) = data_only_wire();
    for read_len in READ_LENS {
        let (sync_data, sync_count) = drive_sync_stack(&wire, read_len);
        let (async_data, async_count) =
            drive_async_stack(Cursor::new(wire.clone()), read_len).await;

        assert_eq!(
            sync_data, expected,
            "sync stack delivered wrong bytes at read_len {read_len}"
        );
        assert_eq!(
            async_data, sync_data,
            "async stack DATA diverged from sync at read_len {read_len}"
        );
        assert_eq!(
            async_count, sync_count,
            "async byte counter diverged from sync at read_len {read_len} \
             (sync={sync_count}, async={async_count})"
        );
        // Both drained the whole stream, so the counter equals the wire length
        // exactly: no buffering prefetch skew, the known feature-on quirk absent.
        assert_eq!(
            async_count,
            wire.len() as u64,
            "async byte count must equal exact wire length at read_len {read_len}"
        );
    }
}

/// Same end-to-end stack parity, but the async transport yields bytes in tiny
/// chunks so every frame is reassembled across many `.await` points. The counter
/// must still land byte-exact regardless of how the transport fragments reads.
#[tokio::test(flavor = "current_thread")]
async fn reader_stack_parity_chunked_delivery() {
    let (wire, expected) = data_only_wire();
    for read_len in READ_LENS {
        let (sync_data, sync_count) = drive_sync_stack(&wire, read_len);
        for chunk in [1usize, 2, 3, 7, 13] {
            let reader = ChunkedReader {
                inner: Cursor::new(wire.clone()),
                chunk,
            };
            let (async_data, async_count) = drive_async_stack(reader, read_len).await;

            assert_eq!(
                async_data, expected,
                "async stack DATA diverged with chunk {chunk}, read_len {read_len}"
            );
            assert_eq!(
                async_data, sync_data,
                "async stack DATA diverged from sync with chunk {chunk}, read_len {read_len}"
            );
            assert_eq!(
                async_count, sync_count,
                "async byte counter diverged from sync with chunk {chunk}, read_len {read_len}"
            );
            assert_eq!(
                async_count,
                wire.len() as u64,
                "async byte count must equal exact wire length with chunk {chunk}, \
                 read_len {read_len}"
            );
        }
    }
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

/// Drives the multiplex-mode [`AsyncServerReader`] purely through its
/// [`tokio::io::AsyncRead`] impl (`read().await`, never the inherent
/// `read_async`), returning the demuxed bytes. This is the entry point the
/// follow-up async read leaves will use: a uniform `R: AsyncRead` source.
///
/// `read_len` is the caller buffer size, exercised small so multi-frame data is
/// split across `poll_read` calls exactly as the sync `ServerReader` splits it.
async fn drive_asyncread_impl<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    read_len: usize,
) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut server = AsyncServerReader::new_plain(reader)
        .activate_multiplex()
        .expect("activate multiplex");

    let mut data = Vec::new();
    let mut buf = vec![0u8; read_len];
    loop {
        // `AsyncRead::read` returns Ok(0) on EOF; the demux surfaces a truncated
        // trailing frame as UnexpectedEof, identical to the sync driver, so both
        // terminators preserve parity.
        match server.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => panic!("AsyncRead impl read failed: {err}"),
        }
    }
    data
}

/// The new [`tokio::io::AsyncRead`] impl for [`AsyncServerReader`] must yield the
/// exact same demuxed bytes the sync [`ServerReader`] yields for the same wire,
/// across read-buffer sizes. This is the poll-driven half of the parity gate: it
/// proves the `poll_read` core delivers the identical stream the inherent
/// `read_async` and blocking `Read` paths do.
#[tokio::test(flavor = "current_thread")]
async fn asyncread_impl_parity_whole_stream() {
    let (wire, expected) = data_only_wire();
    for read_len in READ_LENS {
        let (sync_data, _sync_count) = drive_sync_stack(&wire, read_len);
        let async_data = drive_asyncread_impl(Cursor::new(wire.clone()), read_len).await;

        assert_eq!(
            sync_data, expected,
            "sync stack delivered wrong bytes at read_len {read_len}"
        );
        assert_eq!(
            async_data, sync_data,
            "AsyncRead impl DATA diverged from sync at read_len {read_len}"
        );
    }
}

/// Same byte-identity guarantee for the `AsyncRead` impl, but the transport
/// yields bytes in tiny chunks so frames reassemble across many `poll_read`
/// calls - and, on a real socket, across `Poll::Pending` returns. This is the
/// load-bearing test for the impl's Pending-safety: the in-flight demux future
/// must survive fragmentation without dropping or reordering bytes.
#[tokio::test(flavor = "current_thread")]
async fn asyncread_impl_parity_chunked_delivery() {
    let (wire, expected) = data_only_wire();
    for read_len in READ_LENS {
        let (sync_data, _sync_count) = drive_sync_stack(&wire, read_len);
        for chunk in [1usize, 2, 3, 7, 13] {
            let reader = ChunkedReader {
                inner: Cursor::new(wire.clone()),
                chunk,
            };
            let async_data = drive_asyncread_impl(reader, read_len).await;
            assert_eq!(
                async_data, expected,
                "AsyncRead impl DATA diverged with chunk {chunk}, read_len {read_len}"
            );
            assert_eq!(
                async_data, sync_data,
                "AsyncRead impl DATA diverged from sync with chunk {chunk}, read_len {read_len}"
            );
        }
    }
}

/// The `AsyncRead` impl in plain (non-multiplex) mode is a pure pass-through: it
/// must deliver the raw transport bytes unchanged, matching the sync
/// [`ServerReader`] plain path. Covers the `Plain` arm of `poll_read` alongside
/// the multiplex arm exercised by the other tests.
#[tokio::test(flavor = "current_thread")]
async fn asyncread_impl_plain_passthrough() {
    use tokio::io::AsyncReadExt;
    let raw: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    for read_len in READ_LENS {
        let mut server = AsyncServerReader::new_plain(Cursor::new(raw.clone()));
        assert!(!server.is_multiplexed(), "plain mode before activation");

        let mut data = Vec::new();
        let mut buf = vec![0u8; read_len];
        loop {
            match server.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(err) => panic!("plain AsyncRead read failed: {err}"),
            }
        }
        assert_eq!(
            data, raw,
            "plain-mode AsyncRead pass-through diverged at read_len {read_len}"
        );
    }
}

/// Builds a compressed multiplexed wire: deflate `payload`, then frame the
/// compressed bytes into one or more `MSG_DATA` frames (split at `frame_split`
/// so the compressed stream spans multiple demuxed frames, matching a real
/// multi-frame transfer). The receiver demuxes the frames, concatenates the
/// compressed bytes, and inflates them back to `payload`.
fn compressed_multiplex_wire(payload: &[u8], frame_split: usize) -> Vec<u8> {
    use compress::zlib::{CompressionLevel, compress_to_vec};

    let compressed = compress_to_vec(payload, CompressionLevel::Default).unwrap();
    let mut wire = Vec::new();
    let mut off = 0;
    while off < compressed.len() {
        let end = (off + frame_split.max(1)).min(compressed.len());
        protocol::send_msg(
            &mut wire,
            protocol::MessageCode::Data,
            &compressed[off..end],
        )
        .unwrap();
        off = end;
    }
    wire
}

/// Drives the **sync** compressed receiver path over `wire`:
/// `ServerReader` in multiplex mode, then `activate_compression(Zlib)`, so the
/// inner `CompressedReader<MultiplexReader<_>>` demuxes and inflates. Returns the
/// decompressed bytes.
fn drive_sync_compressed(wire: &[u8], read_len: usize) -> Vec<u8> {
    use compress::algorithm::CompressionAlgorithm;

    let mut server = ServerReader::new_plain(Cursor::new(wire.to_vec()))
        .activate_multiplex()
        .expect("activate multiplex")
        .activate_compression(CompressionAlgorithm::Zlib)
        .expect("activate compression");

    let mut data = Vec::new();
    let mut buf = vec![0u8; read_len];
    loop {
        // The multiplex loop surfaces end-of-wire as UnexpectedEof (a frame read
        // past the last frame), identical for both drivers - see the demux
        // harness above. Terminating on it preserves parity.
        match server.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => panic!("sync compressed read failed: {err}"),
        }
    }
    data
}

/// Drives the **async** compressed twin over `reader`:
/// `AsyncServerReader` in multiplex mode (the async demux, whose control-frame
/// side-effect ordering is parity-proven elsewhere) feeding
/// `AsyncCompressedReader`, which inflates the demuxed compressed bytes with the
/// shared sync decoder. Returns the decompressed bytes.
async fn drive_async_compressed<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    read_len: usize,
) -> Vec<u8> {
    use compress::algorithm::CompressionAlgorithm;

    let mut server = AsyncServerReader::new_plain(reader)
        .activate_multiplex()
        .expect("activate multiplex");
    let mut decompressor = AsyncCompressedReader::new(CompressionAlgorithm::Zlib);

    let mut data = Vec::new();
    let mut buf = vec![0u8; read_len];
    loop {
        match decompressor.read_async(&mut server, &mut buf).await {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(err) => panic!("async compressed read failed: {err}"),
        }
    }
    data
}

/// End-to-end sync-vs-async parity for the compressed reader layer: the async
/// twin (async multiplex demux -> `AsyncCompressedReader` sync inflate) must
/// deliver the byte-identical decompressed stream the sync compressed path
/// delivers, and both must equal the original payload. Covers the required
/// compressed round-trip across read-buffer sizes and multi-frame splits.
#[tokio::test(flavor = "current_thread")]
async fn compressed_round_trip_parity_whole_stream() {
    // A payload large enough to compress across several demuxed frames and to
    // span many small decompressed reads.
    let mut payload = Vec::new();
    for i in 0..4000u32 {
        payload.extend_from_slice(format!("line {i} of the compressed corpus\n").as_bytes());
    }

    for frame_split in [37usize, 512, 65536] {
        let wire = compressed_multiplex_wire(&payload, frame_split);
        for read_len in READ_LENS {
            let sync_data = drive_sync_compressed(&wire, read_len);
            let async_data = drive_async_compressed(Cursor::new(wire.clone()), read_len).await;

            assert_eq!(
                sync_data, payload,
                "sync compressed path lost bytes (frame_split {frame_split}, read_len {read_len})"
            );
            assert_eq!(
                async_data, sync_data,
                "async compressed twin diverged from sync (frame_split {frame_split}, \
                 read_len {read_len})"
            );
        }
    }
}

/// Same compressed round-trip, but the async transport yields bytes in tiny
/// chunks so the compressed frames reassemble across many `.await` points before
/// decode. The decompressed output must stay byte-identical to the sync path
/// regardless of transport fragmentation.
#[tokio::test(flavor = "current_thread")]
async fn compressed_round_trip_parity_chunked_delivery() {
    let mut payload = Vec::new();
    for i in 0..2000u32 {
        payload.extend_from_slice(format!("chunk {i} payload data\n").as_bytes());
    }

    let frame_split = 200usize;
    let wire = compressed_multiplex_wire(&payload, frame_split);
    let sync_data = drive_sync_compressed(&wire, 64);

    for chunk in [1usize, 2, 3, 7, 13] {
        for read_len in READ_LENS {
            let reader = ChunkedReader {
                inner: Cursor::new(wire.clone()),
                chunk,
            };
            let async_data = drive_async_compressed(reader, read_len).await;
            assert_eq!(
                async_data, payload,
                "async compressed twin lost bytes (chunk {chunk}, read_len {read_len})"
            );
            assert_eq!(
                async_data, sync_data,
                "async compressed twin diverged from sync (chunk {chunk}, read_len {read_len})"
            );
        }
    }
}
