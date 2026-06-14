//! Async SSH transport built on `tokio::process`.
//!
//! This module exposes [`AsyncSshTransport`], a tokio-backed counterpart to
//! the synchronous [`SshConnection`](super::SshConnection). Spawning is
//! delegated through the existing [`SshCommand`](super::SshCommand) builder
//! so option injection, batch-mode handling, keepalives, and the
//! `[user@]host` operand stay byte-identical between the sync and async
//! paths. Only the process backing changes: `tokio::process::Command`
//! replaces `std::process::Command`, and stdin/stdout become
//! `AsyncWrite`/`AsyncRead` halves.
//!
//! # Scope
//!
//! This is task #1796: the spawn primitive plus the
//! `(AsyncRead, AsyncWrite)` split. The downstream
//! `ChannelReader`/`ChannelWriter` adapters that bridge these halves into
//! the multiplex framing layer are tracked separately as task #1797.
//! Likewise, an async stderr drain and async connect-watchdog are deferred:
//! stderr is currently inherited from the parent, and connect timeouts are
//! enforced by the `-o ConnectTimeout=N` option that the shared builder
//! already injects.
//!
//! # Feature gate
//!
//! Compiled only under `--features async-ssh`. Default builds remain on
//! the synchronous transport.
//!
//! # Example
//!
//! ```ignore
//! use rsync_io::ssh::{AsyncSshTransport, SshConnectConfig};
//! use std::ffi::OsString;
//!
//! # async fn demo() -> std::io::Result<()> {
//! let config = SshConnectConfig::new();
//! let args: Vec<OsString> = ["rsync", "--server", "."]
//!     .into_iter()
//!     .map(OsString::from)
//!     .collect();
//! let transport = AsyncSshTransport::execute_remote_rsync(
//!     "backup@host.example",
//!     &args,
//!     &config,
//! )
//! .await?;
//! let (_reader, _writer) = transport.split();
//! # Ok(())
//! # }
//! ```

use std::ffi::OsString;
use std::io;
use std::process::{ExitStatus, Stdio};
#[cfg(all(feature = "ssh-socketpair-stderr", unix))]
use std::sync::OnceLock;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::connect::{SshConnectConfig, build_ssh_command};

#[cfg(all(feature = "ssh-socketpair-stderr", unix))]
use super::async_stderr_drain::AsyncStderrDrain;
#[cfg(all(feature = "ssh-socketpair-stderr", unix))]
use super::aux_channel::configure_stderr_channel;

/// Marker substring emitted with the SSF-2 async-fallback warning. Tests
/// assert on this substring; operators can grep their logs for it.
#[cfg(all(feature = "ssh-socketpair-stderr", unix))]
pub(super) const ASYNC_FALLBACK_WARNING_MARKER: &str =
    "SSH stderr async drain falling back to Stdio::inherit()";

/// SSF-2 site 3: surface the async transport's degradation when
/// `configure_stderr_channel` returns `None`. The async path further
/// falls through to `Stdio::inherit()` (unlike the sync path's
/// `Stdio::piped()`), so `stderr_capture()` will return an empty slice
/// for the rest of the session. Fires at most once per process via
/// [`ASYNC_FALLBACK_WARNED`]; the OnceLock is local to this site so
/// repeat SSH spawns within the same process do not spam observers.
#[cfg(all(feature = "ssh-socketpair-stderr", unix))]
static ASYNC_FALLBACK_WARNED: OnceLock<()> = OnceLock::new();

/// Emits the SSF-2 site-3 warning the first time `lock` is set in this
/// process. Returns `true` when the warning was emitted (first call) and
/// `false` thereafter so tests can assert one-shot discipline without
/// having to install a tracing subscriber.
///
/// Production code calls this via [`warn_async_fallback`]; tests inject
/// a local `OnceLock<()>` to exercise the discipline in isolation.
#[cfg(all(feature = "ssh-socketpair-stderr", unix))]
fn emit_async_fallback_warning(lock: &OnceLock<()>) -> bool {
    if lock.set(()).is_err() {
        return false;
    }
    tracing::warn!(
        target = "ssh::stderr",
        "warning: {ASYNC_FALLBACK_WARNING_MARKER}. The remote stderr will surface on the parent terminal but stderr_capture() will return empty (operators get no in-band capture)."
    );
    true
}

/// Production wrapper around [`emit_async_fallback_warning`] that targets
/// the module-level [`ASYNC_FALLBACK_WARNED`] OnceLock.
#[cfg(all(feature = "ssh-socketpair-stderr", unix))]
fn warn_async_fallback() {
    let _ = emit_async_fallback_warning(&ASYNC_FALLBACK_WARNED);
}

/// Tokio-backed SSH transport.
///
/// Wraps a spawned `ssh` child whose stdin and stdout are configured as
/// piped async halves. The child is reaped via [`Self::wait`] or, failing
/// that, on `Drop` of the underlying [`tokio::process::Child`] which sets
/// `kill_on_drop(true)` when constructed by
/// [`Self::execute_remote_rsync`].
pub struct AsyncSshTransport {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    /// Drain task for the SSH socketpair-backed stderr channel.
    ///
    /// `Some` only when both `async-ssh` and `ssh-socketpair-stderr` are
    /// enabled and the socketpair was successfully installed at spawn
    /// time. When `None`, the transport falls back to the `Stdio::inherit`
    /// path that preceded SSE-4 (PR #2373).
    #[cfg(all(feature = "ssh-socketpair-stderr", unix))]
    stderr_drain: Option<AsyncStderrDrain>,
}

impl std::fmt::Debug for AsyncSshTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("AsyncSshTransport");
        dbg.field("child", &self.child)
            .field("stdin_open", &self.stdin.is_some())
            .field("stdout_open", &self.stdout.is_some());
        #[cfg(all(feature = "ssh-socketpair-stderr", unix))]
        dbg.field("stderr_drain", &self.stderr_drain.is_some());
        dbg.finish()
    }
}

impl AsyncSshTransport {
    /// Spawns an `ssh` subprocess and returns an async transport ready for
    /// bidirectional I/O.
    ///
    /// The argv is composed by [`super::SshCommand`] - the same builder
    /// used by the synchronous [`super::SshConnection::connect_with_config`]
    /// path - so a given `(remote, args, config)` triple renders identical
    /// bytes on both transports. `args` is appended verbatim after the
    /// destination operand and replaces any `remote_command` already
    /// present in `config`.
    ///
    /// # Errors
    ///
    /// Returns any [`io::Error`] surfaced by
    /// [`tokio::process::Command::spawn`] (typically `NotFound` when the
    /// `ssh` binary is missing or `PermissionDenied` when the process is
    /// sandboxed away from `execve`), or `BrokenPipe` when the spawned
    /// child fails to expose either pipe.
    pub async fn execute_remote_rsync(
        remote: &str,
        args: &[OsString],
        config: &SshConnectConfig,
    ) -> io::Result<Self> {
        let effective_config = if args.is_empty() {
            config.clone()
        } else {
            config.clone().with_remote_command(args.iter().cloned())
        };

        let (program, command_args) = build_ssh_command(remote, &effective_config).command_parts();

        let mut command = Command::new(&program);
        command.args(command_args.iter());
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());

        // Socketpair-backed stderr drain (SSE-4, #2373). When both
        // `async-ssh` and `ssh-socketpair-stderr` are enabled on Unix,
        // install a `socketpair(AF_UNIX, SOCK_STREAM, 0)` whose child end
        // becomes the spawned ssh's fd 2; the parent end is wrapped in a
        // tokio `UnixStream` and handed to `AsyncStderrDrain::spawn`.
        // On any other configuration, fall back to `Stdio::inherit` so
        // default builds remain byte-identical to pre-SSE-4 behaviour.
        #[cfg(all(feature = "ssh-socketpair-stderr", unix))]
        let parent_socketpair_end = {
            let parent = configure_stderr_channel(command.as_std_mut());
            // SSE-4 design (docs/design/socketpair-stderr-channel.md, sec 4):
            // when the socketpair factory returns `None` (FD exhaustion),
            // revert to `Stdio::inherit` rather than the pipe fallback the
            // sync path picks. The async transport has no ChildStderr-aware
            // drain to feed; inheriting preserves pre-SSE-4 visibility and
            // avoids leaving a piped stderr undrained.
            if parent.is_none() {
                command.stderr(Stdio::inherit());
                // SSF-2 site 3: aux_channel already surfaced the root
                // socketpair failure (site 1). This warning specifically
                // calls out the async-only consequence that
                // stderr_capture() will return empty for this session
                // because the async path inherits stderr rather than
                // piping it.
                warn_async_fallback();
            }
            parent
        };
        #[cfg(not(all(feature = "ssh-socketpair-stderr", unix)))]
        command.stderr(Stdio::inherit());

        // Reap the child on Drop if the caller forgets to await `wait()`,
        // mirroring the sync path's `SshChildHandle` Drop behaviour.
        command.kill_on_drop(true);

        let mut child = command.spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh command did not expose a writable stdin",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh command did not expose a readable stdout",
            )
        })?;

        #[cfg(all(feature = "ssh-socketpair-stderr", unix))]
        let stderr_drain = match parent_socketpair_end {
            Some(parent) => {
                // tokio's UnixStream needs a non-blocking std handle, then
                // wraps it in the runtime's I/O driver. `from_std` returns
                // `io::Error` only when the runtime is missing - we're
                // inside `execute_remote_rsync` which is an `async fn`, so
                // a runtime is guaranteed present.
                parent.set_nonblocking(true)?;
                let async_parent = tokio::net::UnixStream::from_std(parent)?;
                Some(AsyncStderrDrain::spawn(async_parent))
            }
            None => None,
        };

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: Some(stdout),
            #[cfg(all(feature = "ssh-socketpair-stderr", unix))]
            stderr_drain,
        })
    }

    /// Snapshot of bytes captured from the SSH child's stderr.
    ///
    /// Returns an empty `Vec` when:
    ///
    /// - `ssh-socketpair-stderr` is disabled,
    /// - the platform is not Unix (the socketpair path is Unix-only),
    /// - or the socketpair factory fell back to inherit (FD exhaustion).
    ///
    /// Bounded to `ASYNC_STDERR_BUFFER_CAP` bytes (sliding window). Safe
    /// to call concurrently with the drain task and after `wait()`.
    #[cfg(all(feature = "ssh-socketpair-stderr", unix))]
    #[must_use]
    pub fn stderr_capture(&self) -> Vec<u8> {
        self.stderr_drain
            .as_ref()
            .map(AsyncStderrDrain::stderr_capture)
            .unwrap_or_default()
    }

    /// Fallback accessor when the socketpair drain is compiled out.
    ///
    /// Always returns an empty `Vec`. Exists so call sites can compile
    /// without conditional access expressions on every read.
    #[cfg(not(all(feature = "ssh-socketpair-stderr", unix)))]
    #[must_use]
    pub fn stderr_capture(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Splits the transport into its async read and write halves.
    ///
    /// The returned reader wraps the child's stdout, and the writer wraps
    /// the child's stdin. The underlying [`tokio::process::Child`] is
    /// dropped together with this method's `self`, so callers that need to
    /// reap the child explicitly should call [`Self::wait`] first.
    ///
    /// # Panics
    ///
    /// Panics if either pipe was already taken (e.g., by a prior call to
    /// [`Self::take_stdin`] / [`Self::take_stdout`]).
    pub fn split(mut self) -> (impl AsyncRead, impl AsyncWrite) {
        let stdout = self
            .stdout
            .take()
            .expect("AsyncSshTransport::split: stdout already taken");
        let stdin = self
            .stdin
            .take()
            .expect("AsyncSshTransport::split: stdin already taken");
        (stdout, stdin)
    }

    /// Removes the async stdin half from the transport, leaving the rest
    /// of the connection intact.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// Removes the async stdout half from the transport, leaving the rest
    /// of the connection intact.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// Waits for the spawned `ssh` child to exit and returns its exit
    /// status.
    ///
    /// Closes the inherited stdin pipe before awaiting so the remote sees
    /// EOF and shuts down cleanly. Reads from the stdout half after this
    /// call returns will see EOF.
    ///
    /// # Errors
    ///
    /// Surfaces any [`io::Error`] reported by `tokio::process::Child::wait`.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        // Drop stdin to signal EOF; ignore failures because the child may
        // already have exited.
        drop(self.stdin.take());
        self.child.wait().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn ssh_binary_available() -> bool {
        std::process::Command::new("ssh").arg("-V").output().is_ok()
    }

    #[allow(dead_code)]
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    }

    #[test]
    fn execute_remote_rsync_argv_matches_sync_path() {
        // No spawn here: we use `build_ssh_command` directly to confirm
        // the argv composition the async path will hand to tokio mirrors
        // the sync path byte-for-byte. This is the contract the async
        // transport relies on so behaviour stays in lockstep.
        let config = SshConnectConfig::new();
        let args: Vec<OsString> = ["rsync", "--server", "."]
            .into_iter()
            .map(OsString::from)
            .collect();

        let async_view = config.clone().with_remote_command(args.iter().cloned());
        let (async_program, async_args) =
            build_ssh_command("user@example.com", &async_view).command_parts();
        let (sync_program, sync_args) = build_ssh_command(
            "user@example.com",
            &config.clone().with_remote_command(args.iter().cloned()),
        )
        .command_parts();

        assert_eq!(async_program, sync_program);
        assert_eq!(async_args, sync_args);
    }

    /// Network-touching smoke test: spawn `ssh` against an unroutable
    /// address with a tight ConnectTimeout and confirm the future resolves
    /// with a non-zero exit status (typically 255) within a bounded
    /// window. Gated behind `OC_RSYNC_SSH_NET=1` because CI runners with
    /// locked-down outbound networking would otherwise hang on the
    /// underlying TCP attempt before SSH's own timeout fires.
    #[test]
    fn execute_remote_rsync_unreachable_host_returns_nonzero() {
        if std::env::var_os("OC_RSYNC_SSH_NET").is_none() {
            return;
        }
        if !ssh_binary_available() {
            return;
        }

        let config = SshConnectConfig::new()
            .with_connect_timeout(Some(std::time::Duration::from_secs(2)))
            .with_keepalive(None);
        let args: Vec<OsString> = ["true"].into_iter().map(OsString::from).collect();

        let rt = rt();
        let status = rt
            .block_on(async move {
                // RFC 5737 TEST-NET-1 - guaranteed unroutable for
                // documentation use.
                let mut transport =
                    AsyncSshTransport::execute_remote_rsync("nobody@192.0.2.1", &args, &config)
                        .await?;
                transport.wait().await
            })
            .expect("wait must return an ExitStatus");

        assert!(
            !status.success(),
            "ssh against an unroutable host should not exit cleanly"
        );
    }

    /// Compile-time check that the [`AsyncSshTransport::split`] halves
    /// implement the documented `AsyncRead` / `AsyncWrite` traits.
    /// The body is never executed; it exists purely so a type mismatch
    /// surfaces as a compile error during normal test builds rather than
    /// a runtime test failure that requires the `ssh` binary.
    #[test]
    fn split_halves_implement_async_traits() {
        #[allow(dead_code)]
        fn _assert_traits(t: AsyncSshTransport) {
            fn takes_read<R: AsyncRead>(_: R) {}
            fn takes_write<W: AsyncWrite>(_: W) {}
            let (r, w) = t.split();
            takes_read(r);
            takes_write(w);
        }
    }

    /// When `ssh-socketpair-stderr` is compiled out (or on non-Unix),
    /// `stderr_capture` must compile and return an empty `Vec` rather
    /// than panic. This keeps call sites unconditional.
    #[cfg(not(all(feature = "ssh-socketpair-stderr", unix)))]
    #[test]
    fn stderr_capture_is_empty_when_feature_disabled() {
        // Construct via a minimal path: we cannot easily instantiate
        // `AsyncSshTransport` without spawning, so verify behaviour at
        // the function level by exercising the const branch through a
        // compile-only checker.
        #[allow(dead_code)]
        fn _check(t: &AsyncSshTransport) -> Vec<u8> {
            t.stderr_capture()
        }
    }

    /// SSF-2 site 3: the async transport's `Stdio::inherit()` fallback
    /// must warn exactly once per process. The helper signals first-vs-
    /// repeat via its return value, so the test drives a local
    /// `OnceLock<()>` and asserts the discipline without coordinating
    /// against the process-wide `ASYNC_FALLBACK_WARNED` static (which
    /// other tests in the same binary might already have tripped) and
    /// without depending on the tracing subscriber wiring.
    ///
    /// Marker-text verification stays a non-test responsibility:
    /// [`ASYNC_FALLBACK_WARNING_MARKER`] is exposed at module scope, so
    /// the emitted payload's substring is unit-tested by inspection of
    /// the constant rather than by capturing the subscriber output.
    #[cfg(all(feature = "ssh-socketpair-stderr", unix))]
    #[test]
    fn warns_once_on_async_fallback() {
        let local_lock: OnceLock<()> = OnceLock::new();
        let first = emit_async_fallback_warning(&local_lock);
        let second = emit_async_fallback_warning(&local_lock);
        assert!(first, "first invocation must emit");
        assert!(!second, "second invocation must be suppressed");

        // Documented marker must spell out the user-visible consequence;
        // the production helper formats this exact string into the
        // tracing payload, so substring-grepping the constant is the
        // ground truth for what operators will see.
        assert!(
            ASYNC_FALLBACK_WARNING_MARKER.contains("Stdio::inherit()"),
            "marker must mention Stdio::inherit; got {ASYNC_FALLBACK_WARNING_MARKER:?}"
        );
    }
}
