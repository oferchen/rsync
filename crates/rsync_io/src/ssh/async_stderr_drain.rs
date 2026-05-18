//! Async stderr drain task for the SSH socketpair channel.
//!
//! Gated on `--features async-ssh,ssh-socketpair-stderr`. Spawns a tokio
//! task that copies bytes from the parent end of an SSH stderr socketpair
//! (see `aux_channel.rs::configure_stderr_channel`) into a bounded in-memory
//! ring buffer and emits `tracing::warn!` per line so observers can surface
//! remote SSH diagnostics without burning a dedicated OS thread.
//!
//! Replaces the `Stdio::inherit()` path that `AsyncSshTransport` uses by
//! default. The sync transport's `SocketpairStderrChannel` continues to use
//! a `std::thread`-driven drain; this module is the tokio-native equivalent
//! that lets the async transport reach parity without bringing back a
//! dedicated drain thread per connection.
//!
//! Design reference: `docs/design/socketpair-stderr-channel.md` (SSE-2,
//! tracker #2371), section 4 (AsyncSshTransport integration) and section 5
//! (drain to ring buffer + warning channel).

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::task::JoinHandle;

/// Fixed-capacity byte ring buffer used to cap stderr capture memory.
///
/// Generic over the capacity `N` so the buffer size is a compile-time
/// constant - no heap reallocation on overflow, no runtime length checks
/// against a `usize` cap. The sliding-window discipline matches the sync
/// transport's `STDERR_BUFFER_CAP`: appending past `N` drops the oldest
/// bytes so capture always reflects the most recent `N` bytes of stderr.
///
/// The buffer is intentionally byte-oriented (not line-oriented): SSH and
/// remote rsync may emit non-UTF-8 binary on stderr (locale-encoded
/// messages, raw protocol fragments after a crash), and a line-aware ring
/// would either drop or mangle them.
#[derive(Debug)]
pub struct RingBuffer<const N: usize> {
    /// Backing storage, grown lazily to `N` then re-used in place.
    data: Vec<u8>,
}

impl<const N: usize> RingBuffer<N> {
    /// Creates an empty ring buffer with capacity `N`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Vec::with_capacity(N),
        }
    }

    /// Appends `bytes`, evicting the oldest bytes when total length would
    /// exceed `N`. Sliding-window semantics: after every call,
    /// `self.len() <= N`.
    pub fn extend(&mut self, bytes: &[u8]) {
        // Fast path: appending nothing is a no-op. Avoids touching the
        // backing Vec when callers occasionally hand in empty slices
        // (e.g. read_until on a closed reader before EOF is observed).
        if bytes.is_empty() {
            return;
        }

        // If the incoming chunk alone exceeds capacity, retain only its
        // tail - the same outcome as appending then trimming, but without
        // the intermediate over-allocation.
        if bytes.len() >= N {
            self.data.clear();
            self.data.extend_from_slice(&bytes[bytes.len() - N..]);
            return;
        }

        self.data.extend_from_slice(bytes);
        let len = self.data.len();
        if len > N {
            let excess = len - N;
            self.data.drain(..excess);
        }
    }

    /// Returns a copy of the buffered bytes.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// Returns the current buffered length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` when no bytes are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns the maximum number of bytes the buffer retains.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Capacity of the async drain's ring buffer in bytes.
///
/// 8 KiB is a deliberate choice for the async path: SSH diagnostics that
/// matter (auth failure, host-key mismatch, banner errors, "remote command
/// not found") all fit in well under a kilobyte. A smaller cap than the
/// sync transport's 64 KiB keeps per-connection drain memory bounded when
/// many async transports are multiplexed on a single tokio runtime.
pub const ASYNC_STDERR_BUFFER_CAP: usize = 8192;

/// Tokio-driven stderr drain.
///
/// Owns the join handle for the spawned drain task and a shared bounded
/// `RingBuffer<8192>` that the task writes into. Callers snapshot via
/// [`AsyncStderrDrain::stderr_capture`] after the child has exited.
///
/// Dropping the drain aborts the task; this is safe because the task only
/// borrows the reader end of the socketpair (passed in at `spawn`) and the
/// shared buffer.
pub struct AsyncStderrDrain {
    /// Join handle for the spawned drain task. Wrapped in `Option` so
    /// [`AsyncStderrDrain::join`] can take ownership and `Drop` can abort
    /// when the task is still live.
    handle: Option<JoinHandle<()>>,
    /// Shared bounded buffer mutated by the drain task and read by
    /// [`AsyncStderrDrain::stderr_capture`].
    buffer: Arc<Mutex<RingBuffer<ASYNC_STDERR_BUFFER_CAP>>>,
}

impl std::fmt::Debug for AsyncStderrDrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.buffer.lock().map_or(
            0,
            |buf: std::sync::MutexGuard<'_, RingBuffer<ASYNC_STDERR_BUFFER_CAP>>| buf.len(),
        );
        f.debug_struct("AsyncStderrDrain")
            .field("task_alive", &self.handle.is_some())
            .field("buffered_bytes", &len)
            .finish()
    }
}

impl AsyncStderrDrain {
    /// Spawns the drain task on the current tokio runtime.
    ///
    /// `reader` is the parent end of the socketpair (or pipe) created by
    /// `configure_stderr_channel`. The task reads line-by-line, appends
    /// each line to the bounded ring buffer, and emits a
    /// `tracing::warn!(target = "ssh::stderr", ...)` event with the trimmed
    /// line so structured-log subscribers can surface remote diagnostics
    /// in real time.
    ///
    /// The task terminates cleanly on EOF (the child closed its stderr
    /// endpoint) or on the first `read_until` error.
    pub fn spawn<R>(reader: R) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let buffer: Arc<Mutex<RingBuffer<ASYNC_STDERR_BUFFER_CAP>>> =
            Arc::new(Mutex::new(RingBuffer::new()));
        let task_buffer = Arc::clone(&buffer);

        let handle = tokio::spawn(async move {
            drain_loop(reader, task_buffer).await;
        });

        Self {
            handle: Some(handle),
            buffer,
        }
    }

    /// Returns a snapshot of the bytes drained so far.
    ///
    /// Safe to call concurrently with the drain task. The returned `Vec`
    /// is bounded to [`ASYNC_STDERR_BUFFER_CAP`] bytes.
    #[must_use]
    pub fn stderr_capture(&self) -> Vec<u8> {
        self.buffer
            .lock()
            .map_or_else(|_| Vec::new(), |buf| buf.snapshot())
    }

    /// Awaits the drain task to completion.
    ///
    /// Idempotent: subsequent calls return `Ok(())` immediately. Errors
    /// from `tokio::task::JoinError` (panic or cancellation) are surfaced
    /// so callers can distinguish a clean EOF from a task that died
    /// abnormally.
    ///
    /// # Errors
    ///
    /// Returns the `JoinError` from the underlying tokio task when the
    /// task panicked or was cancelled before reaching EOF.
    pub async fn join(&mut self) -> Result<(), tokio::task::JoinError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        handle.await
    }

    /// Returns `true` while the drain task is still owned by this
    /// instance (i.e., [`Self::join`] has not been called).
    #[must_use]
    pub fn task_alive(&self) -> bool {
        self.handle.is_some()
    }
}

impl Drop for AsyncStderrDrain {
    fn drop(&mut self) {
        // The drain task may still be parked on `read_until` waiting for
        // bytes that will never arrive (e.g. caller aborted before
        // `child.wait()`). Aborting is safe because the task only owns the
        // reader and a clone of the shared buffer; both are tokio/`Arc`
        // primitives that handle abort cleanly.
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Reads `reader` line-by-line into `buffer` and emits a `tracing::warn!`
/// per line until EOF or an I/O error.
///
/// Mirrors the sync `aux_channel::drain_loop` semantics: `read_until(b'\n')`
/// preserves the trailing newline in the buffer, non-UTF-8 lines are
/// lossily decoded for the warn event but their raw bytes still land in
/// the ring buffer, and a zero-byte read terminates the loop (clean EOF).
async fn drain_loop<R>(reader: R, buffer: Arc<Mutex<RingBuffer<ASYNC_STDERR_BUFFER_CAP>>>)
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => break,
            Ok(_) => {
                // Lock-scope kept tight so the snapshot accessor never
                // contends with the per-line write for longer than the
                // extend call itself.
                if let Ok(mut buf) = buffer.lock() {
                    buf.extend(&line);
                }
                let text = String::from_utf8_lossy(&line);
                let trimmed = text.trim_end_matches(|c| c == '\n' || c == '\r');
                if !trimmed.is_empty() {
                    tracing::warn!(target: "ssh::stderr", "{}", trimmed);
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;
    use tokio::time::timeout;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    }

    /// Polls `drain.stderr_capture()` until it contains `needle` or the
    /// async timeout elapses. Returns the matching snapshot on success.
    async fn wait_for_capture(drain: &AsyncStderrDrain, needle: &[u8]) -> Option<Vec<u8>> {
        let deadline = Duration::from_secs(2);
        let result = timeout(deadline, async {
            loop {
                let snap = drain.stderr_capture();
                if snap.windows(needle.len()).any(|w| w == needle) {
                    return snap;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        result.ok()
    }

    #[test]
    fn ring_buffer_extends_and_caps_to_capacity() {
        let mut buf: RingBuffer<8> = RingBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.capacity(), 8);

        buf.extend(b"hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.snapshot(), b"hello");

        buf.extend(b"world!"); // total would be 11, cap is 8 -> retain last 8
        assert_eq!(buf.len(), 8);
        assert_eq!(buf.snapshot(), b"loworld!");
    }

    #[test]
    fn ring_buffer_oversized_single_extend_retains_tail() {
        let mut buf: RingBuffer<4> = RingBuffer::new();
        buf.extend(b"abcdefghij");
        assert_eq!(buf.snapshot(), b"ghij");
    }

    #[test]
    fn ring_buffer_empty_extend_is_noop() {
        let mut buf: RingBuffer<8> = RingBuffer::new();
        buf.extend(b"abc");
        buf.extend(b"");
        assert_eq!(buf.snapshot(), b"abc");
    }

    /// Write 1 KiB into one half of a socketpair; the drain on the other
    /// half must snapshot the same bytes verbatim after EOF.
    #[test]
    fn drain_captures_one_kib_payload() {
        let rt = rt();
        rt.block_on(async {
            let (parent, mut child) = UnixStream::pair().expect("socketpair");
            let mut drain = AsyncStderrDrain::spawn(parent);

            // 1 KiB payload of repeating ASCII so newline-driven reads
            // do not slice it weirdly. Terminate with a newline so the
            // drain loop's read_until returns a single Ok with the full
            // 1 KiB plus newline.
            let mut payload = Vec::with_capacity(1024);
            payload.extend(std::iter::repeat(b'a').take(1023));
            payload.push(b'\n');

            child.write_all(&payload).await.expect("write payload");
            child.shutdown().await.expect("close child end");
            drop(child);

            drain.join().await.expect("drain task completes");

            let snap = drain.stderr_capture();
            assert_eq!(snap, payload, "snapshot must equal what was written");
        });
    }

    /// Multi-line input: each line surfaces as a separate warn event AND
    /// the ring buffer contains every byte the writer sent.
    ///
    /// We assert observability on the ring buffer (the tracing dispatcher
    /// is global and shared with the test harness, so subscribing here
    /// would be flaky); the warn-per-line code path is exercised, and the
    /// byte-level guarantees ensure no line is dropped.
    #[test]
    fn drain_surfaces_each_line_independently() {
        let rt = rt();
        rt.block_on(async {
            let (parent, mut child) = UnixStream::pair().expect("socketpair");
            let mut drain = AsyncStderrDrain::spawn(parent);

            let lines: &[&[u8]] = &[
                b"warning: first line\n",
                b"warning: second line\n",
                b"warning: third line\n",
            ];
            for line in lines {
                child.write_all(line).await.expect("write line");
            }

            // Wait for the last line to materialise in the buffer before
            // closing - guarantees the drain ran read_until once per
            // line rather than coalescing into a single read.
            assert!(
                wait_for_capture(&drain, b"third line").await.is_some(),
                "expected 'third line' to be captured before EOF"
            );

            child.shutdown().await.expect("close child end");
            drop(child);
            drain.join().await.expect("drain task completes");

            let snap = drain.stderr_capture();
            for line in lines {
                assert!(
                    snap.windows(line.len()).any(|w| w == *line),
                    "expected {:?} in captured stderr",
                    std::str::from_utf8(line).unwrap_or("<non-utf8>")
                );
            }
        });
    }

    /// Dropping the writer half must let the drain task terminate cleanly.
    /// `join()` returns `Ok(())` rather than a `JoinError`, proving the
    /// task observed EOF and exited from its `read_until` loop instead of
    /// being aborted by `Drop`.
    #[test]
    fn drop_sender_terminates_drain_task_cleanly() {
        let rt = rt();
        rt.block_on(async {
            let (parent, child) = UnixStream::pair().expect("socketpair");
            let mut drain = AsyncStderrDrain::spawn(parent);

            // No data written. Drop the writer immediately - the drain
            // must observe EOF and complete.
            drop(child);

            let join_result = timeout(Duration::from_secs(2), drain.join())
                .await
                .expect("drain.join must complete within 2s");
            assert!(
                join_result.is_ok(),
                "drain task should exit cleanly on EOF, got {join_result:?}"
            );
            assert!(drain.stderr_capture().is_empty());
        });
    }

    /// Snapshot accessor must be safe to invoke while the drain task is
    /// still running.
    #[test]
    fn snapshot_is_safe_while_drain_is_active() {
        let rt = rt();
        rt.block_on(async {
            let (parent, mut child) = UnixStream::pair().expect("socketpair");
            let mut drain = AsyncStderrDrain::spawn(parent);

            child
                .write_all(b"first chunk\n")
                .await
                .expect("write chunk");
            assert!(wait_for_capture(&drain, b"first chunk").await.is_some());

            // Snapshot while task is still live. Must not panic, must
            // return the bytes seen so far.
            let mid = drain.stderr_capture();
            assert!(
                mid.windows(b"first chunk".len())
                    .any(|w| w == b"first chunk")
            );
            assert!(drain.task_alive());

            child
                .write_all(b"second chunk\n")
                .await
                .expect("write second");
            drop(child);
            drain.join().await.expect("drain completes");

            let final_snap = drain.stderr_capture();
            assert!(
                final_snap
                    .windows(b"second chunk".len())
                    .any(|w| w == b"second chunk")
            );
            assert!(!drain.task_alive());
        });
    }

    /// Writing past the ring-buffer cap must cap the snapshot at
    /// `ASYNC_STDERR_BUFFER_CAP` bytes (sliding-window discipline).
    #[test]
    fn drain_caps_capture_to_ring_buffer_capacity() {
        let rt = rt();
        rt.block_on(async {
            let (parent, mut child) = UnixStream::pair().expect("socketpair");
            let mut drain = AsyncStderrDrain::spawn(parent);

            // Write 2x cap in newline-terminated chunks so the drain
            // loop runs read_until many times rather than reading the
            // whole payload in one go.
            let chunk = vec![b'x'; 1023];
            let mut with_nl = chunk.clone();
            with_nl.push(b'\n');
            let chunks_needed = (ASYNC_STDERR_BUFFER_CAP * 2 / with_nl.len()) + 1;
            for _ in 0..chunks_needed {
                child.write_all(&with_nl).await.expect("write chunk");
            }
            child.shutdown().await.expect("shutdown writer");
            drop(child);
            drain.join().await.expect("drain completes");

            let snap = drain.stderr_capture();
            assert!(
                snap.len() <= ASYNC_STDERR_BUFFER_CAP,
                "captured {} bytes, expected <= {ASYNC_STDERR_BUFFER_CAP}",
                snap.len()
            );
        });
    }
}
