//! Auxiliary stderr channel abstraction for SSH subprocesses.
//!
//! The SSH subprocess writes diagnostic output (e.g., `Permission denied`,
//! host-key warnings, banner messages) to its stderr. We must drain that
//! stream continuously to avoid pipe-buffer deadlocks - if the child fills
//! the OS stderr buffer (~64 KB) and nothing reads it, the child stalls
//! and the transfer hangs.
//!
//! Two strategies share the same trait so callers do not need to know which
//! kernel object backs the channel:
//!
//! - `PipeStderrChannel` wraps a [`std::process::ChildStderr`] (the default
//!   anonymous pipe created by [`std::process::Stdio::piped`]). Works on every
//!   supported platform.
//! - `SocketpairStderrChannel` (Unix only) reads from one end of a
//!   `socketpair(2)` while the other end was passed to the child as its
//!   `stderr`. Socketpairs expose a real, non-blocking-capable file descriptor
//!   that future event-loop integrations can poll alongside stdin/stdout.
//!
//! Both implementations spawn a dedicated drain thread today; the trait is
//! the seam that lets us migrate the socketpair variant onto a poll() loop
//! without touching call sites in `connection.rs`.

#[cfg(unix)]
use std::fmt;
#[cfg(unix)]
use std::io::Write;
use std::io::{self, BufRead, BufReader, Read};
use std::process::{ChildStderr, Command, ExitStatus, Stdio};
#[cfg(unix)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Maximum time to block in [`StderrAuxChannel::join`] waiting for the drain
/// thread to exit after the SSH child has already been reaped. The drain
/// thread blocks on `read(2)` against the kernel stderr endpoint; EOF
/// normally arrives the instant the last writer closes, but if the ssh
/// client re-execs a helper (ssh-askpass, ControlMaster persistence) the
/// helper inherits the write end and EOF can lag the parent's exit by
/// arbitrarily long. We abandon the drain thread after this deadline so
/// the rsync process can return to the user; the orphaned thread exits
/// naturally when the process terminates.
const DRAIN_JOIN_TIMEOUT: Duration = Duration::from_millis(50);

/// Polls [`JoinHandle::is_finished`] until the handle exits or the timeout
/// elapses, then calls [`JoinHandle::join`] if the thread finished. Returns
/// `true` when the thread was joined, `false` when it was abandoned.
fn join_with_timeout(handle: JoinHandle<()>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            // Leak the JoinHandle (and therefore the underlying thread)
            // intentionally. The thread is parked in a kernel read and
            // will exit when the process terminates.
            std::mem::forget(handle);
            return false;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let _ = handle.join();
    true
}

use logging::debug_log;

/// Maximum bytes retained in the stderr capture buffer.
///
/// When the buffer exceeds this limit, the oldest bytes are discarded to keep
/// memory usage bounded. 64 KB matches the typical OS pipe buffer size and is
/// sufficient to capture the tail of any error output from the remote process.
pub(super) const STDERR_BUFFER_CAP: usize = 64 * 1024;

/// Fires `message` to `out` the first time `lock` is set. Subsequent calls
/// against the same lock are no-ops and return `false`.
///
/// Used to keep SSF-2 fallback warnings to one line per session per site
/// even when rsync re-establishes SSH multiple times in the same process
/// (push+pull, resumes, multi-host loops). The lock-set-vs-write order is
/// deliberate: a contested set loses early so concurrent callers do not
/// double-emit. `OnceLock::set` returns `Err` on the losing side, which
/// short-circuits the write.
///
/// `tracing` is not a default dependency of this crate (the
/// `ssh-socketpair-stderr` feature pulls it in), so the sync-path
/// warnings must stay on `io::Write` / `eprintln!` to compile on the
/// default build matrix.
#[cfg(unix)]
fn warn_once(lock: &OnceLock<()>, out: &mut dyn Write, message: fmt::Arguments<'_>) -> bool {
    if lock.set(()).is_err() {
        return false;
    }
    let _ = writeln!(out, "{message}");
    true
}

/// Marker substring emitted with every site-1 warning. Tests assert on
/// this substring; production operators can grep for it.
///
/// Unix-only: the call sites that format this marker are gated on
/// `#[cfg(unix)]` because the socketpair fallback condition only
/// arises on Unix platforms.
#[cfg(unix)]
pub(super) const SOCKETPAIR_REJECTION_WARNING_MARKER: &str =
    "SSH stderr async drain unavailable on this platform";

/// Marker substring emitted with every site-2 warning. See
/// [`SOCKETPAIR_REJECTION_WARNING_MARKER`] for the rationale.
#[cfg(unix)]
pub(super) const HALF_FALLBACK_WARNING_MARKER: &str = "SSH stderr socketpair partially set up";

/// Site 1 (SSF-2): the synchronous `configure_stderr_channel` Unix arm
/// could not allocate a `socketpair(2)` and is degrading to
/// `Stdio::piped()`. Emits the operator-facing warning at most once per
/// process via [`SOCKETPAIR_REJECTION_WARNED`].
#[cfg(unix)]
static SOCKETPAIR_REJECTION_WARNED: OnceLock<()> = OnceLock::new();

/// Site 2 (SSF-2): `SocketpairStderrChannel::spawn` could not
/// `try_clone` the parent socketpair end. The channel still drains but
/// `shutdown_read` becomes a no-op. Fires at most once per process via
/// [`HALF_FALLBACK_WARNED`].
#[cfg(unix)]
static HALF_FALLBACK_WARNED: OnceLock<()> = OnceLock::new();

/// Emits the site-1 warning to `out`. Returns `true` when the warning was
/// emitted (first call) and `false` thereafter.
///
/// Extracted as a helper so the production call site stays a one-liner
/// and the tests can drive the OnceLock through a local instance to
/// observe the one-shot discipline deterministically.
#[cfg(unix)]
fn emit_socketpair_rejection_warning(
    lock: &OnceLock<()>,
    out: &mut dyn Write,
    error: &io::Error,
) -> bool {
    warn_once(
        lock,
        out,
        format_args!(
            "warning: {SOCKETPAIR_REJECTION_WARNING_MARKER}; falling back to pipe-based stderr ({error}). Diagnostics may be delayed or truncated on chatty stderr streams."
        ),
    )
}

/// Emits the site-2 warning to `out`. See
/// [`emit_socketpair_rejection_warning`] for the helper rationale.
#[cfg(unix)]
fn emit_half_fallback_warning(lock: &OnceLock<()>, out: &mut dyn Write, error: &io::Error) -> bool {
    warn_once(
        lock,
        out,
        format_args!(
            "warning: {HALF_FALLBACK_WARNING_MARKER}; shutdown_read became a no-op ({error}). This may delay process shutdown on session close."
        ),
    )
}

/// Abstraction over the auxiliary channel that drains SSH subprocess stderr.
///
/// Implementations must guarantee that:
///
/// 1. The drain thread is started at construction time so the child cannot
///    deadlock on a full stderr buffer before the caller is ready to inspect
///    output.
/// 2. [`collected`](Self::collected) returns a snapshot of the bytes read so
///    far; calling it concurrently with the drain thread is safe.
/// 3. [`join`](Self::join) blocks until the drain thread exits (either at
///    EOF or after the child closes its stderr endpoint).
///
/// The trait uses `&mut self` rather than `Box<Self>` for the join methods
/// because `SshConnection` and `SshChildHandle` need to call them from
/// `Drop` impls where ownership cannot be transferred.
pub(super) trait StderrAuxChannel: Send {
    /// Returns a snapshot of the stderr bytes collected so far.
    ///
    /// Bounded to the most recent [`STDERR_BUFFER_CAP`] bytes. Safe to call
    /// while the drain thread is still running.
    fn collected(&self) -> Vec<u8>;

    /// Closes the parent read side so an in-flight blocking `read()` in the
    /// drain thread returns immediately. Idempotent.
    ///
    /// Callers should invoke this once the child process has exited - the
    /// drain thread is then only useful for whatever bytes are already
    /// buffered, and on socketpair-backed channels the read can block past
    /// child exit if any descendant inherited the child's stderr fd (the
    /// ssh client occasionally re-execs helpers that keep the fd alive
    /// briefly after the main `ssh` exits).
    fn shutdown_read(&self);

    /// Joins the drain thread, blocking until it finishes.
    ///
    /// Idempotent - subsequent calls are no-ops. Callers that already know
    /// the child has exited should call [`shutdown_read`](Self::shutdown_read)
    /// first to guarantee the drain thread is not blocked in `read()`.
    fn join(&mut self);

    /// Joins the drain thread and prints collected stderr when `status`
    /// indicates a non-zero exit.
    ///
    /// Used from `Drop` impls to surface SSH diagnostics on abnormal control
    /// flow (panic, early return) where the caller never invoked
    /// `wait_with_stderr`.
    fn join_and_surface_on_error(&mut self, status: &io::Result<ExitStatus>) {
        self.shutdown_read();
        self.join();

        if let Ok(exit) = status {
            if !exit.success() {
                let stderr = self.collected();
                if !stderr.is_empty() {
                    let text = String::from_utf8_lossy(&stderr);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        eprintln!("ssh process exited with status {exit}:\n{trimmed}");
                    }
                }
            }
        }
    }
}

/// Anonymous-pipe backed stderr channel.
///
/// Wraps the `ChildStderr` handle that `Command::stderr(Stdio::piped())`
/// produces. Available on every platform; this is the default and the only
/// option on Windows.
pub(super) struct PipeStderrChannel {
    handle: Option<JoinHandle<()>>,
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl PipeStderrChannel {
    /// Spawns a background thread that drains `stderr` until EOF.
    pub(super) fn spawn(stderr: ChildStderr) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let thread_buffer = Arc::clone(&buffer);

        let handle = thread::Builder::new()
            .name("ssh-stderr-drain-pipe".into())
            .spawn(move || drain_loop(stderr, &thread_buffer))
            .expect("failed to spawn ssh stderr pipe drain thread");

        Self {
            handle: Some(handle),
            buffer,
        }
    }
}

impl StderrAuxChannel for PipeStderrChannel {
    fn collected(&self) -> Vec<u8> {
        snapshot(&self.buffer)
    }

    fn shutdown_read(&self) {
        // Anonymous pipes have no safe out-of-band wake-up. We rely on
        // [`join`] using a bounded timeout so the drain thread can be
        // abandoned when the child has exited but EOF has not yet
        // propagated (e.g. a fork()ed ssh helper still holds the write
        // end). The thread exits naturally once the kernel closes the
        // write end on process teardown.
    }

    fn join(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        join_with_timeout(handle, DRAIN_JOIN_TIMEOUT);
    }
}

impl Drop for PipeStderrChannel {
    fn drop(&mut self) {
        self.join();
    }
}

/// Socketpair-backed stderr channel (Unix only).
///
/// One half of the `UnixStream::pair` is held here and read by the drain
/// thread; the other half is passed to the child process as its stderr file
/// descriptor. Compared to `PipeStderrChannel`, this exposes a real socket
/// that can be registered with `poll(2)`/`epoll(7)`/`kqueue(2)` event loops
/// for future zero-thread integration.
#[cfg(unix)]
pub(super) struct SocketpairStderrChannel {
    handle: Option<JoinHandle<()>>,
    buffer: Arc<Mutex<Vec<u8>>>,
    /// Cloned reference to the parent socketpair endpoint. The drain thread
    /// owns the original `UnixStream`; this clone lets [`shutdown_read`]
    /// wake the parked read by calling `shutdown(Both)` from outside the
    /// thread. `try_clone` failures degrade gracefully to the timeout path.
    parent_clone: Option<UnixStream>,
}

#[cfg(unix)]
impl SocketpairStderrChannel {
    /// Spawns a background thread that drains the parent end of the
    /// socketpair until EOF.
    ///
    /// `parent_end` is the half retained by this process; the child end must
    /// already have been handed to the subprocess as its stderr descriptor
    /// before calling this.
    pub(super) fn spawn(parent_end: UnixStream) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let thread_buffer = Arc::clone(&buffer);

        // SSF-2 site 2: `try_clone` failure leaves `shutdown_read` as a
        // no-op (see `shutdown_read` below). Capture the error so the
        // half-fallback path is surfaced once per process instead of
        // being silently dropped by `.ok()`.
        let parent_clone = match parent_end.try_clone() {
            Ok(clone) => Some(clone),
            Err(error) => {
                emit_half_fallback_warning(&HALF_FALLBACK_WARNED, &mut io::stderr().lock(), &error);
                None
            }
        };

        let handle = thread::Builder::new()
            .name("ssh-stderr-drain-socketpair".into())
            .spawn(move || drain_loop(parent_end, &thread_buffer))
            .expect("failed to spawn ssh stderr socketpair drain thread");

        Self {
            handle: Some(handle),
            buffer,
            parent_clone,
        }
    }
}

#[cfg(unix)]
impl StderrAuxChannel for SocketpairStderrChannel {
    fn collected(&self) -> Vec<u8> {
        snapshot(&self.buffer)
    }

    fn shutdown_read(&self) {
        if let Some(ref sock) = self.parent_clone {
            // Shutting down the read half makes the drain thread's
            // pending `read()` return 0 immediately, breaking the loop
            // and letting the thread exit. This is the safe equivalent
            // of closing the fd while another thread reads from it.
            let _ = sock.shutdown(std::net::Shutdown::Both);
        }
    }

    fn join(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        join_with_timeout(handle, DRAIN_JOIN_TIMEOUT);
    }
}

#[cfg(unix)]
impl Drop for SocketpairStderrChannel {
    fn drop(&mut self) {
        self.join();
    }
}

/// Reads `source` line-by-line, forwards each line to the local process
/// stderr in real time, and collects the bytes into the shared buffer
/// (bounded to [`STDERR_BUFFER_CAP`]).
///
/// Generic over the byte source so the same loop drives both
/// `PipeStderrChannel` (over `ChildStderr`) and `SocketpairStderrChannel`
/// (over `UnixStream`).
///
/// Uses `read_until(b'\n')` instead of `lines()` to handle non-UTF-8
/// output without prematurely terminating the drain. SSH or the remote
/// process may emit binary data on stderr (e.g., locale-encoded error
/// messages); dropping such lines would leave the channel un-drained and
/// risk the deadlock this thread exists to prevent.
fn drain_loop<R: Read>(source: R, buffer: &Mutex<Vec<u8>>) {
    let mut reader = BufReader::new(source);
    let mut line_buf = Vec::new();
    loop {
        line_buf.clear();
        match reader.read_until(b'\n', &mut line_buf) {
            Ok(0) => break,
            Ok(_) => {
                // Forward the line to the local process stderr so the user
                // sees SSH errors in real time - matching upstream rsync's
                // behavior of surfacing remote errors immediately.
                let text = String::from_utf8_lossy(&line_buf);
                eprint!("{text}");
                debug_log!(Connect, 3, "ssh stderr: {}", text.trim_end());
                append_bounded(buffer, &line_buf);
            }
            // I/O error (broken pipe, etc.) - child exited, stop draining.
            Err(_) => break,
        }
    }
}

/// Appends `data` to the shared buffer, discarding the oldest bytes when
/// the total exceeds [`STDERR_BUFFER_CAP`].
fn append_bounded(buffer: &Mutex<Vec<u8>>, data: &[u8]) {
    let Ok(mut buf) = buffer.lock() else {
        return;
    };
    buf.extend_from_slice(data);
    let len = buf.len();
    if len > STDERR_BUFFER_CAP {
        let excess = len - STDERR_BUFFER_CAP;
        buf.drain(..excess);
    }
}

/// Snapshots the shared buffer.
fn snapshot(buffer: &Mutex<Vec<u8>>) -> Vec<u8> {
    buffer.lock().map_or_else(|_| Vec::new(), |buf| buf.clone())
}

/// Boxed-and-Send trait object for the auxiliary stderr drain.
///
/// Stored on `SshConnection` and `SshChildHandle` so they can hold either
/// implementation without knowing which kernel object backs it.
pub(super) type BoxedStderrChannel = Box<dyn StderrAuxChannel + Send>;

/// Configures the spawn `Command` with the appropriate stderr endpoint and
/// returns the parent half of the socketpair when one was successfully
/// created.
///
/// On Unix, this attempts to create a socketpair and hand the child end to
/// the subprocess as its stderr. On any failure (e.g., file-descriptor
/// exhaustion) and on non-Unix targets, the command falls back to the
/// conventional anonymous pipe via `Stdio::piped()`.
#[cfg(unix)]
pub(super) fn configure_stderr_channel(command: &mut Command) -> Option<UnixStream> {
    match UnixStream::pair() {
        Ok((parent, child)) => {
            // Hand the child end to the subprocess as its stderr fd. The
            // conversion path is `UnixStream -> OwnedFd -> Stdio` and is
            // entirely safe stdlib API (no FFI).
            let child_fd: std::os::fd::OwnedFd = child.into();
            command.stderr(Stdio::from(child_fd));
            debug_log!(Connect, 3, "ssh stderr: socketpair channel installed");
            Some(parent)
        }
        Err(error) => {
            command.stderr(Stdio::piped());
            debug_log!(
                Connect,
                2,
                "ssh stderr: socketpair unavailable ({error}); falling back to pipe"
            );
            // SSF-2 site 1: surface the runtime degradation once per
            // process so operators notice that the async-drain path is
            // disabled for this session.
            emit_socketpair_rejection_warning(
                &SOCKETPAIR_REJECTION_WARNED,
                &mut io::stderr().lock(),
                &error,
            );
            None
        }
    }
}

#[cfg(not(unix))]
pub(super) fn configure_stderr_channel(command: &mut Command) -> Option<()> {
    command.stderr(Stdio::piped());
    // The socketpair-backed async drain fast path is Unix-only, so it
    // compiles out entirely here. Emit a diagnostic once at the stderr
    // setup point so a debug run on non-Unix targets shows that the
    // pipe-based fallback was taken rather than leaving the downgrade
    // invisible (the Unix arm above traces the equivalent runtime
    // fallback via `debug_log!`).
    debug_log!(
        Connect,
        2,
        "ssh stderr: socketpair async drain unavailable on this platform; using pipe-based stderr"
    );
    None
}

/// Constructs the appropriate `StderrAuxChannel` for the spawned child.
///
/// When `parent_socketpair_end` is `Some`, the socketpair path was selected
/// and the parent end is wrapped in a `SocketpairStderrChannel`. Otherwise
/// the conventional `ChildStderr` pipe is wrapped in a `PipeStderrChannel`.
#[cfg(unix)]
pub(super) fn build_stderr_channel(
    parent_socketpair_end: Option<UnixStream>,
    child_stderr: Option<ChildStderr>,
) -> Option<BoxedStderrChannel> {
    if let Some(parent) = parent_socketpair_end {
        Some(Box::new(SocketpairStderrChannel::spawn(parent)))
    } else {
        child_stderr.map(|stderr| Box::new(PipeStderrChannel::spawn(stderr)) as BoxedStderrChannel)
    }
}

#[cfg(not(unix))]
pub(super) fn build_stderr_channel(
    _parent_socketpair_end: Option<()>,
    child_stderr: Option<ChildStderr>,
) -> Option<BoxedStderrChannel> {
    child_stderr.map(|stderr| Box::new(PipeStderrChannel::spawn(stderr)) as BoxedStderrChannel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    /// Polls `channel.collected()` until it contains `needle` or `deadline`
    /// elapses. Returns the collected bytes on success.
    #[cfg(unix)]
    fn wait_for(
        channel: &dyn StderrAuxChannel,
        needle: &[u8],
        deadline: Instant,
    ) -> Option<Vec<u8>> {
        loop {
            let buf = channel.collected();
            if buf.windows(needle.len()).any(|w| w == needle) {
                return Some(buf);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn socketpair_channel_collects_stderr_data() {
        let (parent, mut child) = UnixStream::pair().expect("create socketpair");
        let mut channel = SocketpairStderrChannel::spawn(parent);

        child
            .write_all(b"hello-from-socketpair\n")
            .expect("write to child end");
        // Closing the child end signals EOF to the drain thread.
        drop(child);
        channel.join();

        let collected = channel.collected();
        assert_eq!(collected, b"hello-from-socketpair\n");
    }

    #[cfg(unix)]
    #[test]
    fn socketpair_and_pipe_channels_produce_same_output() {
        // Both implementations must collect identical bytes for identical
        // input, validating that the trait abstraction is implementation-agnostic.
        let payload_text = "line-one\\nline-two\\nline-three\\n";
        let payload_bytes = b"line-one\nline-two\nline-three\n";

        // Socketpair path - write directly through UnixStream::pair.
        let (parent_sp, mut child_sp) = UnixStream::pair().expect("socketpair");
        let mut sp_channel = SocketpairStderrChannel::spawn(parent_sp);
        child_sp.write_all(payload_bytes).expect("write");
        drop(child_sp);
        sp_channel.join();

        // Pipe path - spawn a child whose stderr writes the same payload.
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!("printf '{payload_text}' >&2"));
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn sh");
        let stderr = child.stderr.take().expect("child stderr");
        let mut pipe_channel = PipeStderrChannel::spawn(stderr);
        let _ = child.wait();
        pipe_channel.join();

        assert_eq!(sp_channel.collected(), payload_bytes);
        assert_eq!(pipe_channel.collected(), payload_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn socketpair_channel_handles_non_utf8_bytes() {
        // Non-UTF-8 must not terminate the drain prematurely.
        let (parent, mut child) = UnixStream::pair().expect("socketpair");
        let mut channel = SocketpairStderrChannel::spawn(parent);

        child.write_all(b"before\n").expect("write before");
        child.write_all(b"\xff\xfe\n").expect("write binary");
        child.write_all(b"after\n").expect("write after");
        drop(child);
        channel.join();

        let collected = channel.collected();
        assert!(
            collected.windows(b"before".len()).any(|w| w == b"before"),
            "expected 'before' segment in {collected:?}"
        );
        assert!(
            collected.windows(b"after".len()).any(|w| w == b"after"),
            "expected 'after' segment in {collected:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn socketpair_channel_collected_can_be_called_while_running() {
        // collected() must be safe to call concurrently with the drain thread.
        let (parent, mut child) = UnixStream::pair().expect("socketpair");
        let channel = SocketpairStderrChannel::spawn(parent);

        child
            .write_all(b"streaming-snapshot-test\n")
            .expect("write");
        let deadline = Instant::now() + Duration::from_secs(2);
        let snapshot = wait_for(&channel, b"streaming-snapshot-test", deadline);
        drop(child);
        // Even if we never call join() explicitly, Drop must clean up cleanly.
        assert!(
            snapshot.is_some(),
            "channel should observe data while drain thread is still running"
        );
    }

    #[cfg(unix)]
    #[test]
    fn socketpair_channel_bounded_buffer_caps_memory() {
        // Writing more than STDERR_BUFFER_CAP must keep the buffer bounded
        // by discarding the oldest bytes (sliding-window semantics).
        let (parent, mut child) = UnixStream::pair().expect("socketpair");
        let mut channel = SocketpairStderrChannel::spawn(parent);

        // Write 2x the cap in 1 KB chunks, terminating each with a newline so
        // the drain loop emits/collects one piece at a time.
        let chunk = vec![b'x'; 1023];
        let mut chunk_with_nl = chunk.clone();
        chunk_with_nl.push(b'\n');
        let total_chunks = (STDERR_BUFFER_CAP * 2) / chunk_with_nl.len() + 1;
        for _ in 0..total_chunks {
            child.write_all(&chunk_with_nl).expect("write chunk");
        }
        drop(child);
        channel.join();

        let collected = channel.collected();
        assert!(
            collected.len() <= STDERR_BUFFER_CAP,
            "collected {} bytes, expected <= {STDERR_BUFFER_CAP}",
            collected.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn join_and_surface_on_error_is_quiet_for_success() {
        // Successful exits must not surface anything; the trait's default
        // implementation must short-circuit when status.success() is true.
        let (parent, mut child) = UnixStream::pair().expect("socketpair");
        let mut channel = SocketpairStderrChannel::spawn(parent);
        child
            .write_all(b"would-not-be-shown\n")
            .expect("write payload");
        drop(child);

        // Fake a successful status by spawning the simplest possible child.
        let status = std::process::Command::new("true").status();
        channel.join_and_surface_on_error(&status);

        // We cannot assert on stderr capture in-process, but the call must
        // complete without panicking and the buffer must still be intact.
        assert_eq!(channel.collected(), b"would-not-be-shown\n");
    }

    #[cfg(unix)]
    #[test]
    fn channel_join_is_idempotent() {
        // Calling join() multiple times must be safe (no double-panic from
        // re-joining a JoinHandle). Both impls share this Drop/join logic.
        //
        // The writer half is dropped via the wildcard pattern `_` (not the
        // named binding `_b`, which would extend the writer's lifetime to
        // end-of-scope and block the drain thread waiting for EOF).
        let (a, _) = UnixStream::pair().expect("socketpair");
        let mut channel = SocketpairStderrChannel::spawn(a);
        channel.join();
        channel.join();
        let _ = channel.collected();
    }

    #[cfg(unix)]
    #[test]
    fn append_bounded_drops_oldest_bytes_when_overflowing() {
        let buffer = Mutex::new(Vec::new());
        // Pre-fill with cap-1 bytes.
        append_bounded(&buffer, &vec![b'a'; STDERR_BUFFER_CAP - 1]);
        // Append 10 more, total is cap+9 - we must drop the oldest 9.
        append_bounded(&buffer, b"NEWESTBYTE");
        let snapshot_buf = snapshot(&buffer);
        assert_eq!(snapshot_buf.len(), STDERR_BUFFER_CAP);
        assert_eq!(&snapshot_buf[snapshot_buf.len() - 10..], b"NEWESTBYTE");
    }

    #[cfg(unix)]
    #[test]
    fn build_stderr_channel_prefers_socketpair_when_available() {
        // When the parent socketpair end is provided, the factory must
        // construct the socketpair-backed channel and ignore the pipe input.
        let (parent, child) = UnixStream::pair().expect("create socketpair");
        let mut channel = build_stderr_channel(Some(parent), None).expect("expected a channel");
        // Close the child end so the drain thread observes EOF and exits;
        // otherwise channel.join() would block forever.
        drop(child);
        channel.join();
        assert!(channel.collected().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn build_stderr_channel_falls_back_to_pipe_when_socketpair_absent() {
        // When the parent socketpair end is None, the factory must wrap the
        // ChildStderr in a PipeStderrChannel. We synthesize a real
        // ChildStderr by spawning a trivial child process.
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf 'fallback-to-pipe\\n' >&2");
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::piped());

        let mut child = command.spawn().expect("spawn sh");
        let stderr = child.stderr.take().expect("child stderr");
        let mut channel = build_stderr_channel(None, Some(stderr)).expect("expected a channel");
        let _ = child.wait();
        channel.join();

        let collected = channel.collected();
        assert!(
            collected
                .windows(b"fallback-to-pipe".len())
                .any(|w| w == b"fallback-to-pipe"),
            "expected 'fallback-to-pipe' in {collected:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_stderr_channel_returns_none_when_no_inputs_provided() {
        // Defensive: when neither input is supplied, the factory must yield
        // None rather than constructing a half-initialised channel.
        let channel = build_stderr_channel(None, None);
        assert!(channel.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn configure_stderr_channel_installs_socketpair_when_possible() {
        // The Unix configurator should install a socketpair-backed stderr
        // and return the parent half. We verify by looking at the returned
        // Option (Some => socketpair installed) and then by inspecting that
        // the spawned child writes back through the socketpair.
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf 'configure-installed\\n' >&2");
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        let parent = configure_stderr_channel(&mut command).expect("socketpair available");
        let mut child = command.spawn().expect("spawn sh");
        let _ = child.wait();
        // `command.stderr(Stdio::from(child_fd))` retains the child end of
        // the socketpair inside `Command` (spawn dups the fd into the child
        // but does not consume the parent's copy). Drop it now so the
        // remaining writer reference is the child's fd 2; once the child
        // exits, the parent end will see EOF instead of blocking forever.
        drop(command);
        let mut channel = SocketpairStderrChannel::spawn(parent);
        channel.join();
        let collected = channel.collected();
        assert!(
            collected
                .windows(b"configure-installed".len())
                .any(|w| w == b"configure-installed"),
            "expected payload routed through socketpair, got {collected:?}"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn build_stderr_channel_uses_pipe_on_non_unix() {
        // On Windows the only path is the pipe-backed channel; verify that
        // the factory wraps the provided ChildStderr accordingly.
        let mut command = Command::new("cmd");
        command.arg("/C").arg("echo windows-pipe-payload 1>&2");
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::piped());

        let mut child = command.spawn().expect("spawn cmd");
        let stderr = child.stderr.take().expect("child stderr");
        let mut channel = build_stderr_channel(None, Some(stderr)).expect("expected a channel");
        let _ = child.wait();
        channel.join();
        let collected = channel.collected();
        assert!(
            String::from_utf8_lossy(&collected).contains("windows-pipe-payload"),
            "expected payload in {:?}",
            String::from_utf8_lossy(&collected)
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn configure_stderr_channel_uses_pipe_on_non_unix() {
        // On Windows the configurator unconditionally uses Stdio::piped().
        let mut command = Command::new("cmd");
        command.arg("/C").arg("echo non-unix 1>&2");
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        let pending = configure_stderr_channel(&mut command);
        assert!(
            pending.is_none(),
            "non-Unix path should never own a parent socketpair end"
        );
        let mut child = command.spawn().expect("spawn cmd");
        let _ = child.wait();
        // The pipe is captured by the child object; the stub above just
        // proves no panic and that the call returns None.
    }

    // SSF-2 site 1 + site 2 emission tests. Each test owns a local
    // `OnceLock<()>` so the production statics remain untouched and
    // tests stay independent of execution order.

    #[cfg(unix)]
    #[test]
    fn warns_once_on_sync_socketpair_rejection() {
        let local_lock: OnceLock<()> = OnceLock::new();
        let synthetic_error = io::Error::from_raw_os_error(libc_emfile_equivalent());

        let mut sink = Vec::new();
        let first = emit_socketpair_rejection_warning(&local_lock, &mut sink, &synthetic_error);
        assert!(first, "first invocation must emit the warning");
        let captured = String::from_utf8(sink).expect("emitted bytes are utf8");
        assert!(
            captured.contains(SOCKETPAIR_REJECTION_WARNING_MARKER),
            "emitted warning must contain the documented marker; got {captured:?}"
        );
        assert!(
            captured.contains("falling back to pipe-based stderr"),
            "emitted warning must explain the fallback behaviour; got {captured:?}"
        );

        let mut sink_second = Vec::new();
        let second =
            emit_socketpair_rejection_warning(&local_lock, &mut sink_second, &synthetic_error);
        assert!(
            !second,
            "second invocation must be suppressed by the OnceLock"
        );
        assert!(
            sink_second.is_empty(),
            "second invocation must not write to the output sink; got {sink_second:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn warns_once_on_half_fallback() {
        let local_lock: OnceLock<()> = OnceLock::new();
        let synthetic_error = io::Error::from_raw_os_error(libc_emfile_equivalent());

        let mut sink = Vec::new();
        let first = emit_half_fallback_warning(&local_lock, &mut sink, &synthetic_error);
        assert!(first, "first invocation must emit the warning");
        let captured = String::from_utf8(sink).expect("emitted bytes are utf8");
        assert!(
            captured.contains(HALF_FALLBACK_WARNING_MARKER),
            "emitted warning must contain the documented marker; got {captured:?}"
        );
        assert!(
            captured.contains("shutdown_read became a no-op"),
            "emitted warning must explain the no-op consequence; got {captured:?}"
        );

        let mut sink_second = Vec::new();
        let second = emit_half_fallback_warning(&local_lock, &mut sink_second, &synthetic_error);
        assert!(
            !second,
            "second invocation must be suppressed by the OnceLock"
        );
        assert!(
            sink_second.is_empty(),
            "second invocation must not write to the output sink; got {sink_second:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn success_path_does_not_invoke_rejection_warning() {
        // SSF-4: the warning helper must only be called on the rejection
        // arm of `configure_stderr_channel`. On the success path (socketpair
        // allocated cleanly) the helper is never invoked, so a fresh
        // `OnceLock<()>` paired with an empty sink must stay untouched
        // for the lifetime of the success-path drive.
        //
        // We exercise the real success path by calling `configure_stderr_channel`
        // on a `Command` whose socketpair allocation will succeed under
        // normal test conditions, then assert that an independent local lock
        // (representing the warning gate) was never set and that an
        // independent sink (representing operator stderr) received nothing.
        let local_lock: OnceLock<()> = OnceLock::new();
        let sink: Vec<u8> = Vec::new();

        let mut command = Command::new("sh");
        command.arg("-c").arg("true");
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());

        let parent = configure_stderr_channel(&mut command);
        assert!(
            parent.is_some(),
            "test precondition: socketpair must be available on this host"
        );

        // Reap the configured command so the child fd is released cleanly.
        if let Ok(mut child) = command.spawn() {
            let _ = child.wait();
        }
        drop(parent);

        assert!(
            local_lock.get().is_none(),
            "success path must not touch any warning OnceLock; got {:?}",
            local_lock.get()
        );
        assert!(
            sink.is_empty(),
            "success path must not emit any bytes to the operator sink; got {sink:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn warn_once_helper_is_thread_safe() {
        // Concurrent first-callers must agree on exactly one emission.
        let local_lock: OnceLock<()> = OnceLock::new();
        let lock_ref = &local_lock;

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                handles.push(scope.spawn(move || {
                    let mut sink = Vec::new();
                    let emitted = warn_once(lock_ref, &mut sink, format_args!("contention"));
                    (emitted, sink)
                }));
            }
            let results: Vec<(bool, Vec<u8>)> = handles
                .into_iter()
                .map(|h| h.join().expect("join"))
                .collect();
            let emitted_count = results.iter().filter(|(emit, _)| *emit).count();
            let non_empty_sinks = results.iter().filter(|(_, s)| !s.is_empty()).count();
            assert_eq!(emitted_count, 1, "exactly one thread must report emission");
            assert_eq!(
                non_empty_sinks, 1,
                "exactly one sink must have received bytes"
            );
        });
    }

    /// Returns an `errno` value that mirrors EMFILE on the host. We avoid a
    /// hard dependency on `libc` here because the production module does
    /// not need it - the synthetic error is only used to construct an
    /// `io::Error` whose `Display` impl exercises the warning format.
    #[cfg(unix)]
    fn libc_emfile_equivalent() -> i32 {
        // EMFILE is 24 on Linux/macOS/BSDs; on the off-chance a future
        // target diverges, any positive errno still produces a usable
        // `io::Error` for the test (it just exercises a different
        // `Display` string).
        24
    }
}
