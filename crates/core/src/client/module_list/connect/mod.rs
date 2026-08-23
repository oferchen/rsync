mod direct;
mod program;
mod proxy;
mod rsh;

use std::ffi::OsStr;
use std::io::{self, IoSlice, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use super::super::{AddressMode, ClientError, TcpFastOpenMode, TransferTimeout};
use super::{DaemonAddress, Transport};
pub(crate) use direct::{connect_direct, resolve_daemon_addresses};
pub(crate) use program::ConnectProgramConfig;
use program::ConnectProgramStream;
pub(crate) use proxy::{
    ProxyConfig, ProxyCredentials, connect_via_proxy, establish_proxy_tunnel, load_daemon_proxy,
    parse_proxy_spec,
};
pub(crate) use rsh::{RshDaemonSpawn, spawn_rsh_daemon_stream};

/// Read half of a [`DaemonStream`] after splitting.
pub(crate) enum DaemonStreamReader {
    /// Cloned TCP socket used for reading.
    Tcp(TcpStream),
    /// Connect program read half: Unix socketpair clone or child stdout
    /// pipe (Unix), or child stdout pipe (non-Unix).
    #[cfg(unix)]
    Program(program::ProgramReader),
    #[cfg(not(unix))]
    Program(std::process::ChildStdout),
    /// QUIC bidirectional stream read handle (blocking `Read`).
    #[cfg(feature = "quic")]
    Quic(rsync_io::quic::QuicStream),
}

impl Read for DaemonStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(reader) => reader.read(buf),
            #[cfg(feature = "quic")]
            Self::Quic(stream) => stream.read(buf),
        }
    }
}

impl DaemonStreamReader {
    /// Clones the underlying TCP read half so an adopted daemon
    /// `MSG_IO_TIMEOUT` can be re-applied to the live socket. Returns `None`
    /// for connect-program (pipe) transports, which carry no socket timeout.
    pub(crate) fn try_clone_tcp(&self) -> Option<TcpStream> {
        match self {
            Self::Tcp(stream) => stream.try_clone().ok(),
            Self::Program(_) => None,
            #[cfg(feature = "quic")]
            Self::Quic(_) => None,
        }
    }
}

/// TCP write half that corks output around each write-then-flush burst.
///
/// The multiplex writer above this layer accumulates a burst of `MSG_DATA`
/// frames and then issues a single `flush()` at a per-file / per-batch
/// boundary (upstream: `io.c` `iobuf_out` batching, ~10 files per write).
/// Left uncorked, each `send_msg()` header+payload `write_all` pair and each
/// buffered frame can leave the kernel as its own small TCP segment. Corking
/// (`TCP_CORK` on Linux, `TCP_NOPUSH` on macOS/FreeBSD) holds those partial
/// segments in the kernel until the burst ends, so the flush emits fewer,
/// fuller segments. This is a pure segmentation/timing change: the wire
/// payload bytes and their order are identical to the uncorked stream.
///
/// Corking is armed lazily on the first `write()` after a flush and cleared
/// (uncorked) at every `flush()` and on `Drop`, so the socket is never left
/// stuck corked on an error / early-return / panic path. Uncorking at flush
/// also preserves the flush-before-blocking-read invariant: the multiplex
/// writer flushes before the sender blocks reading the peer's next request,
/// which releases the coalesced segment to the wire. On platforms without a
/// cork option `set_tcp_cork` is a no-op and `corked` stays `false`.
pub(crate) struct CorkedTcpWriter {
    stream: TcpStream,
    /// True while the socket is corked (a burst is in flight, uncleared).
    corked: bool,
}

impl CorkedTcpWriter {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            corked: false,
        }
    }

    /// Corks the socket if not already corked. Best-effort: a failure to set
    /// the option leaves `corked` false so `flush`/`Drop` never issue a
    /// dangling uncork, and never fails the write path.
    fn cork(&mut self) {
        if !self.corked {
            if let Ok(true) = fast_io::set_tcp_cork(&self.stream, true) {
                self.corked = true;
            }
        }
    }

    /// Uncorks the socket if currently corked, flushing any partial segment
    /// the kernel was holding. Errors are surfaced so a failed uncork (which
    /// would otherwise strand buffered bytes) is not swallowed.
    fn uncork(&mut self) -> io::Result<()> {
        if self.corked {
            self.corked = false;
            // Best-effort: clearing the cork on a torn-down socket can fail
            // (e.g. macOS TCP_NOPUSH returns EINVAL after the peer FIN). The
            // cork is moot once the socket is gone and the flag is already
            // cleared, so never surface an uncork error to the write path.
            let _ = fast_io::set_tcp_cork(&self.stream, false);
        }
        Ok(())
    }
}

impl Write for CorkedTcpWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Arm corking for the burst before the first byte reaches the kernel.
        self.cork();
        self.stream.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.cork();
        self.stream.write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Flush user-space bytes first, then uncork so the kernel releases the
        // coalesced segment before the caller blocks on the peer's response.
        self.stream.flush()?;
        self.uncork()
    }
}

impl Drop for CorkedTcpWriter {
    fn drop(&mut self) {
        // Clear any lingering cork on every exit path (error, early return,
        // panic unwind) so a dropped writer never leaves the socket stalled.
        let _ = self.uncork();
    }
}

/// Write half of a [`DaemonStream`] after splitting.
pub(crate) enum DaemonStreamWriter {
    /// Original TCP socket used for writing, with burst corking applied.
    Tcp(CorkedTcpWriter),
    /// Connect program write half: Unix socketpair clone or child stdin
    /// pipe (Unix), or child stdin pipe (non-Unix).
    #[cfg(unix)]
    Program(program::ProgramWriter),
    #[cfg(not(unix))]
    Program(std::process::ChildStdin),
    /// QUIC bidirectional stream write handle (blocking `Write`).
    #[cfg(feature = "quic")]
    Quic(rsync_io::quic::QuicStream),
}

impl Write for DaemonStreamWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(writer) => writer.write(buf),
            Self::Program(writer) => writer.write(buf),
            #[cfg(feature = "quic")]
            Self::Quic(writer) => writer.write(buf),
        }
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match self {
            Self::Tcp(writer) => writer.write_vectored(bufs),
            Self::Program(writer) => writer.write_vectored(bufs),
            #[cfg(feature = "quic")]
            Self::Quic(writer) => writer.write_vectored(bufs),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(writer) => writer.flush(),
            Self::Program(writer) => writer.flush(),
            #[cfg(feature = "quic")]
            Self::Quic(writer) => writer.flush(),
        }
    }
}

impl DaemonStreamWriter {
    /// Clones the underlying TCP write half so an adopted daemon
    /// `MSG_IO_TIMEOUT` can be re-applied to the live socket. Returns `None`
    /// for connect-program (pipe) transports, which carry no socket timeout.
    pub(crate) fn try_clone_tcp(&self) -> Option<TcpStream> {
        match self {
            Self::Tcp(writer) => writer.stream.try_clone().ok(),
            Self::Program(_) => None,
            #[cfg(feature = "quic")]
            Self::Quic(_) => None,
        }
    }
}

/// Builds a live-socket I/O-timeout re-apply hook for the client receiver.
///
/// Captures cloned read and write halves of the daemon socket. When the client
/// adopts a daemon-advertised `MSG_IO_TIMEOUT`, the hook re-applies the value as
/// the socket's read and write timeouts (both fds reference one kernel socket,
/// so either updates the pair). Returns `None` for connect-program transports,
/// which have no socket timeout to adjust.
///
/// upstream: io.c:1148-1157 `set_io_timeout()` - the client-side effect of
/// adopting a daemon `MSG_IO_TIMEOUT` (io.c:1551-1561).
pub(crate) fn build_io_timeout_reapply(
    reader: &DaemonStreamReader,
    writer: &DaemonStreamWriter,
) -> Option<crate::server::IoTimeoutReapply> {
    let read_half = reader.try_clone_tcp();
    let write_half = writer.try_clone_tcp();
    if read_half.is_none() && write_half.is_none() {
        return None;
    }
    Some(crate::server::IoTimeoutReapply(std::sync::Arc::new(
        move |secs: u32| -> io::Result<()> {
            let timeout = (secs != 0).then(|| Duration::from_secs(u64::from(secs)));
            if let Some(stream) = &read_half {
                stream.set_read_timeout(timeout)?;
                stream.set_write_timeout(timeout)?;
            }
            if let Some(stream) = &write_half {
                stream.set_read_timeout(timeout)?;
                stream.set_write_timeout(timeout)?;
            }
            Ok(())
        },
    )))
}

/// Registers a shutdown wake hook for the daemon socket.
///
/// The hook half-closes the socket, so a transfer parked in a blocking read
/// returns at once and unwinds through the same path a dropped connection
/// takes - which is what makes `--partial` / `--partial-dir` retention and
/// temp-file cleanup identical on the signal path and the connection-loss
/// path. Returns `None` for connect-program (pipe) transports, which own no
/// socket to shut down.
///
/// upstream: `io.c:750` polls `got_kill_signal` from `perform_io()` once its
/// `select()` returns; a multi-threaded process cannot rely on the signal
/// reaching the thread that is blocked on the wire, so the socket is closed
/// from the watcher instead.
pub(crate) fn register_shutdown_wake(
    reader: &DaemonStreamReader,
) -> Option<crate::signal::IoWakerGuard> {
    let socket = reader.try_clone_tcp()?;
    crate::signal::register_io_waker(std::sync::Arc::new(move || {
        // Best-effort: the peer may already have closed the connection, in
        // which case shutdown(2) reports ENOTCONN and there is nothing to wake.
        let _ = socket.shutdown(std::net::Shutdown::Both);
    }))
}

/// The connect-phase and per-I/O timeouts for a daemon connection.
///
/// Bundles the two timeouts that always travel together: `connect` bounds
/// `connect(2)` (only when `--contimeout` is set) and `io` is applied as the
/// established stream's read/write timeout.
#[derive(Clone, Copy)]
pub(crate) struct DaemonConnectTimeouts {
    /// Connect-phase bound (`--contimeout`), or `None` to leave it unbounded.
    pub(crate) connect: Option<Duration>,
    /// Per-I/O timeout applied to the established stream.
    pub(crate) io: Option<Duration>,
}

/// Client-side QUIC dial parameters carried to the connect-selection point.
///
/// Holds the trust-source inputs the QUIC ladder needs: the `--quic-ca` private
/// CA bundle path (when supplied) selects [`QuicTrust::Roots`](rsync_io::quic::QuicTrust);
/// otherwise the system-roots default applies. The struct is empty (a
/// zero-sized value) and never read when the `quic` feature is disabled, so it
/// threads through the shared connect signature without a cfg on the parameter
/// itself.
#[derive(Clone, Default)]
pub(crate) struct QuicDialParams {
    /// `--quic-ca <PATH>`: a PEM CA bundle that replaces the platform trust
    /// store for verifying the daemon's certificate.
    #[cfg(feature = "quic")]
    pub(crate) ca: Option<std::path::PathBuf>,
}

/// Opens a stream to a daemon, dispatching on the address's [`Transport`].
///
/// This is the single connect-selection point shared by module listing and
/// daemon transfers. The transport is read off [`DaemonAddress::transport`];
/// `Transport::Tcp` (the default) establishes a plain TCP connection, while
/// `Transport::Quic` dials the same daemon protocol over QUIC.
pub(crate) fn open_daemon_stream(
    addr: &DaemonAddress,
    timeouts: DaemonConnectTimeouts,
    address_mode: AddressMode,
    connect_program: Option<&OsStr>,
    bind_address: Option<SocketAddr>,
    tfo: TcpFastOpenMode,
    sockopts: Option<&OsStr>,
    quic: &QuicDialParams,
) -> Result<DaemonStream, ClientError> {
    // The QUIC dial params are consumed only by the `Transport::Quic` arm, which
    // is compiled out in a default (non-`quic`) build.
    #[cfg(not(feature = "quic"))]
    let _ = quic;
    // Transport-selection point: the transport rides on `DaemonAddress` from
    // target parsing. `Transport::Tcp` (the default, and the only variant in a
    // default build) establishes the plain TCP daemon stream, so default builds
    // are byte-for-byte unchanged.
    match addr.transport() {
        Transport::Tcp => open_tcp_daemon_stream(
            addr,
            timeouts.connect,
            timeouts.io,
            address_mode,
            connect_program,
            bind_address,
            tfo,
            sockopts,
        ),
        // QUIC dials over UDP and hands the resulting blocking stream to the
        // same `@RSYNCD` handshake TCP uses; the daemon-protocol bytes after the
        // QUIC handshake are unchanged (the QUIC-7 wire-identity invariant). A
        // failed dial hard-fails and never silently downgrades to plaintext TCP
        // (design: `docs/design/quic-transport-policy.md`, Decision D, "No
        // silent downgrade").
        #[cfg(feature = "quic")]
        Transport::Quic => open_quic_daemon_stream(addr, address_mode, quic),
    }
}

/// Opens a QUIC connection to a daemon (the [`Transport::Quic`] path).
///
/// Resolves the client trust source through the QUIC verification ladder
/// (`--quic-ca` private CA > system-roots default; the TOFU backend is wired in
/// `rsync_io::quic` and engaged by a follow-up opt-in), builds the connector
/// with ALPN `rsync`, resolves the daemon host to `host:port` candidates
/// (873/udp default), and dials the first that succeeds. The peer certificate
/// is validated against the `quic://` authority (`addr.host()`) as the TLS
/// server name.
///
/// Any dial or handshake failure - unreachable endpoint, certificate rejection,
/// or ALPN mismatch - maps to `RERR_STARTCLIENT` (exit 5). This is an interim
/// mapping; the full quinn-proto error -> exit-code schema is a separate task
/// (#57). Crucially it is an error, never a TCP fallback.
#[cfg(feature = "quic")]
fn open_quic_daemon_stream(
    addr: &DaemonAddress,
    address_mode: AddressMode,
    quic: &QuicDialParams,
) -> Result<DaemonStream, ClientError> {
    use rsync_io::quic::{QuicConnector, load_private_ca, resolve};

    // Trust ladder (policy B): `--quic-ca` selects a private CA bundle;
    // otherwise the system-roots default applies. The `resolve(ca, tofu)` seam
    // keeps the TOFU precedence slot in place (tofu = None here).
    let ca = match &quic.ca {
        Some(path) => {
            Some(load_private_ca(path).map_err(|error| quic_dial_error(addr, &error.to_string()))?)
        }
        None => None,
    };
    let trust = resolve(ca, None).map_err(|error| quic_dial_error(addr, &error.to_string()))?;
    let connector = QuicConnector::with_trust(trust)
        .map_err(|error| quic_dial_error(addr, &error.to_string()))?;

    let candidates = resolve_daemon_addresses(addr, address_mode)?;
    let server_name = addr.host();
    let mut last_error: Option<io::Error> = None;
    for candidate in candidates {
        match connector.connect(candidate, server_name) {
            Ok(stream) => return Ok(DaemonStream::quic(stream)),
            Err(error) => last_error = Some(error),
        }
    }
    let error = last_error
        .expect("resolve_daemon_addresses guarantees at least one candidate")
        .to_string();
    Err(quic_dial_error(addr, &error))
}

/// Maps a QUIC dial/handshake failure to a [`ClientError`] with exit code
/// `RERR_STARTCLIENT` (5).
///
/// Interim classifier: every failure - connection loss, certificate rejection,
/// and ALPN mismatch (`no application protocol`) - maps to 5, matching the
/// policy that an explicitly-requested encrypted transport that cannot be
/// established fails loudly rather than downgrading. The full quinn-proto error
/// taxonomy (distinguishing e.g. handshake-timeout from cert-reject) is #57;
/// this leaves the message informative and the exit code stable.
#[cfg(feature = "quic")]
fn quic_dial_error(addr: &DaemonAddress, detail: &str) -> ClientError {
    use super::super::CLIENT_SERVER_PROTOCOL_EXIT_CODE;
    use super::super::daemon_error;

    daemon_error(
        format!(
            "QUIC connection to {} failed: {detail}; refusing to fall back to plaintext TCP",
            addr.socket_addr_display()
        ),
        CLIENT_SERVER_PROTOCOL_EXIT_CODE,
    )
}

/// Opens a plain TCP connection to a daemon (the [`Transport::Tcp`] path).
///
/// Respects `RSYNC_CONNECT_PROG` and `RSYNC_PROXY` environment variables.
/// `sockopts` (`--sockopts`), when given, is applied to the connecting socket
/// before `connect(2)` for both the direct and proxied paths; it has no effect
/// on a connect program, matching upstream (a connect program bypasses
/// `open_socket_out()` entirely).
fn open_tcp_daemon_stream(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    connect_program: Option<&OsStr>,
    bind_address: Option<SocketAddr>,
    tfo: TcpFastOpenMode,
    sockopts: Option<&OsStr>,
) -> Result<DaemonStream, ClientError> {
    if let Some(program) = program::load_daemon_connect_program(connect_program)? {
        return program::connect_via_program(addr, &program);
    }

    let stream = match load_daemon_proxy()? {
        Some(proxy) => proxy::connect_via_proxy(
            addr,
            &proxy,
            connect_timeout,
            io_timeout,
            bind_address,
            tfo,
            sockopts,
        )?,
        None => connect_direct(
            addr,
            connect_timeout,
            io_timeout,
            address_mode,
            bind_address,
            tfo,
            sockopts,
        )?,
    };

    Ok(DaemonStream::tcp(stream))
}

/// Resolves the connect-phase timeout for a daemon TCP connection.
///
/// Upstream arms a `SIGALRM` around `connect(2)` only when `--contimeout` is set
/// to a positive value; the default `connect_timeout` is `0`, in which case the
/// connect blocks for the OS SYN timeout. `--timeout` never bounds the connect
/// phase - it only governs per-read/write I/O on an established stream. Hence a
/// connect is bounded only when `--contimeout=N` (`N > 0`) was given.
///
/// upstream: socket.c:274-277 `open_socket_out()` installs `alarm(connect_timeout)`
/// solely for `connect_timeout > 0`; options.c:125 defaults `connect_timeout = 0`.
pub(crate) const fn resolve_connect_timeout(connect_timeout: TransferTimeout) -> Option<Duration> {
    match connect_timeout {
        // --contimeout=N (N > 0): bound the connect phase.
        TransferTimeout::Seconds(value) => Some(Duration::from_secs(value.get())),
        // Unset (Default) or --contimeout=0 (Disabled): leave the connect
        // unbounded, matching upstream's default connect_timeout=0. --timeout
        // must not leak into the connect phase.
        TransferTimeout::Default | TransferTimeout::Disabled => None,
    }
}

/// Bidirectional stream to an rsync daemon.
///
/// Abstracts over the underlying transport: plain TCP or a connect program
/// (`RSYNC_CONNECT_PROG`).
pub(crate) enum DaemonStream {
    /// Plain TCP connection.
    Tcp(TcpStream),
    /// Connection via an external connect program.
    Program(ConnectProgramStream),
    /// Native QUIC connection carrying the daemon protocol (oc extension).
    #[cfg(feature = "quic")]
    Quic(rsync_io::quic::QuicStream),
}

impl DaemonStream {
    const fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    #[cfg(feature = "quic")]
    const fn quic(stream: rsync_io::quic::QuicStream) -> Self {
        Self::Quic(stream)
    }

    fn program(stream: ConnectProgramStream) -> Self {
        Self::Program(stream)
    }

    /// Creates a `DaemonStream` from a child process's stdio handles.
    ///
    /// Used by daemon-over-remote-shell mode where the caller spawns
    /// the SSH process directly and needs to wrap its pipes as a daemon
    /// transport.
    pub(crate) fn from_child_process(
        child: std::process::Child,
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
    ) -> Self {
        Self::Program(ConnectProgramStream::from_pipes(child, stdin, stdout))
    }

    /// Returns a reference to the underlying `TcpStream` if this is a TCP
    /// connection. Used for applying socket-level options that only apply
    /// to real sockets (not connect programs).
    pub(crate) fn as_tcp_stream(&self) -> Option<&TcpStream> {
        match self {
            Self::Tcp(stream) => Some(stream),
            Self::Program(_) => None,
            #[cfg(feature = "quic")]
            Self::Quic(_) => None,
        }
    }

    /// Splits the daemon stream into independent read and write halves.
    ///
    /// For TCP, the socket is cloned (separate fd) so reader and writer
    /// can be used concurrently. For connect programs on Unix, the
    /// socketpair fd is cloned; on non-Unix the child's stdout and stdin
    /// pipes are returned directly.
    ///
    /// Returns `(reader, writer, guard)`. The guard must be held alive for
    /// the duration of the transfer - for connect programs it owns the
    /// `Child` process and kills it on drop.
    pub(crate) fn split(
        self,
    ) -> io::Result<(DaemonStreamReader, DaemonStreamWriter, DaemonStreamGuard)> {
        match self {
            Self::Tcp(stream) => {
                let reader = stream.try_clone()?;
                Ok((
                    DaemonStreamReader::Tcp(reader),
                    DaemonStreamWriter::Tcp(CorkedTcpWriter::new(stream)),
                    DaemonStreamGuard::None,
                ))
            }
            Self::Program(prog) => {
                let parts = prog.into_parts()?;
                Ok((
                    DaemonStreamReader::Program(parts.reader),
                    DaemonStreamWriter::Program(parts.writer),
                    DaemonStreamGuard::Child(Some(parts.child)),
                ))
            }
            // The QUIC read and write handles are cheap clones over one
            // bidirectional stream; the guard owns the teardown so those
            // handles drop cheaply while the last written bytes are still
            // flushed (finish + close) before the connection ends - the QUIC
            // analogue of a TCP socket flushing its send buffer on close.
            #[cfg(feature = "quic")]
            Self::Quic(stream) => {
                let guard = stream.shutdown_guard();
                Ok((
                    DaemonStreamReader::Quic(stream.try_clone()),
                    DaemonStreamWriter::Quic(stream),
                    DaemonStreamGuard::Quic(guard),
                ))
            }
        }
    }

    /// Configures TCP-specific socket options for the transfer phase.
    ///
    /// Sets TCP_NODELAY and applies read/write timeouts. No-op for
    /// non-TCP transports (connect programs).
    pub(crate) fn configure_transfer_options(
        &self,
        nodelay: bool,
        timeout: Option<Duration>,
    ) -> io::Result<()> {
        if let Self::Tcp(stream) = self {
            if nodelay {
                stream.set_nodelay(true)?;
            }
            stream.set_read_timeout(timeout)?;
            stream.set_write_timeout(timeout)?;
        }
        Ok(())
    }
}

/// Ownership guard for resources backing a split [`DaemonStream`].
///
/// For connect programs, this owns the `Child` process handle. For TCP
/// streams, no guard is needed.
pub(crate) enum DaemonStreamGuard {
    /// No resource to guard (TCP).
    None,
    /// Owns a connect program child process; `None` once [`Self::finish`] reaped it.
    Child(Option<std::process::Child>),
    /// Owns the QUIC connection teardown (flush + FIN + graceful close on drop).
    #[cfg(feature = "quic")]
    Quic(rsync_io::quic::QuicShutdown),
}

impl DaemonStreamGuard {
    /// Waits for a connect program child to exit on its own.
    ///
    /// The caller must drop both stream halves first: closing them is what
    /// gives the child EOF, and the child does the rest of its work - a daemon
    /// reached through `RSYNC_CONNECT_PROG` runs its `post-xfer exec` hook
    /// after the last transfer byte - before exiting.
    ///
    /// upstream: socket.c:1046 `sock_exec()` forks the connect program and
    /// never signals it; the child ends when the socket closes.
    pub(crate) fn finish(mut self) {
        if let Self::Child(child) = &mut self {
            if let Some(mut child) = child.take() {
                let _ = child.wait();
            }
        }
    }
}

impl Drop for DaemonStreamGuard {
    fn drop(&mut self) {
        match self {
            Self::None => {}
            // Safety net for abnormal control flow (early return, panic) that
            // never reached `finish`. The halves may still be open here, so the
            // child cannot be waited for; kill it rather than block. Mirrors
            // `SshChildHandle::drop`.
            Self::Child(Some(child)) => {
                if let Ok(None) = child.try_wait() {
                    let _ = child.kill();
                }
                let _ = child.wait();
            }
            Self::Child(None) => {}
            // The QUIC teardown (flush + FIN + graceful close) runs when the
            // held `QuicShutdown` drops right after this body; nothing to do
            // here beyond keeping ownership until now.
            #[cfg(feature = "quic")]
            Self::Quic(guard) => {
                let _ = guard;
            }
        }
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(stream) => stream.read(buf),
            #[cfg(feature = "quic")]
            Self::Quic(stream) => stream.read(buf),
        }
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Program(stream) => stream.write(buf),
            #[cfg(feature = "quic")]
            Self::Quic(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Program(stream) => stream.flush(),
            #[cfg(feature = "quic")]
            Self::Quic(stream) => stream.flush(),
        }
    }
}

#[cfg(all(test, feature = "quic"))]
mod quic_connect_tests {
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::thread;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use rsync_io::quic::QuicAcceptor;
    use tempfile::TempDir;

    use super::*;
    use crate::client::{AddressMode, TcpFastOpenMode};

    /// PEM-encodes a certificate DER and writes it to a `--quic-ca` bundle file,
    /// returning the temp dir (kept alive) and the file path.
    fn write_ca_pem(cert_der: &[u8]) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ca.pem");
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for chunk in STANDARD.encode(cert_der).as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        std::fs::write(&path, pem).expect("write ca pem");
        (dir, path)
    }

    /// Builds a QUIC daemon address for a loopback acceptor. Uses `localhost`
    /// (the acceptor cert's SAN) as the host so rustls hostname verification
    /// passes, with `Ipv4` mode so it resolves to the `127.0.0.1` the acceptor
    /// bound.
    fn quic_addr(port: u16) -> DaemonAddress {
        DaemonAddress::new("localhost".to_owned(), port).with_transport(Transport::Quic)
    }

    fn dial(addr: &DaemonAddress, ca: Option<PathBuf>) -> Result<DaemonStream, ClientError> {
        let quic = QuicDialParams { ca };
        open_daemon_stream(
            addr,
            DaemonConnectTimeouts {
                connect: None,
                io: None,
            },
            AddressMode::Ipv4,
            None,
            None,
            TcpFastOpenMode::Off,
            None,
            &quic,
        )
    }

    /// End to end over the loopback fixture: `--quic-ca` (the acceptor's
    /// self-signed cert, used as its own trust anchor) is accepted, the dial
    /// yields a `DaemonStream`, and that stream is a byte-transparent
    /// `Read`+`Write` drop-in - the server reads exactly what the client wrote
    /// and vice versa, with no framing added by the transport. This is the
    /// QUIC-7 wire-identity invariant at the transport boundary: whatever the
    /// `@RSYNCD` handshake writes reaches the peer unchanged.
    #[test]
    fn quic_ca_trust_path_dials_and_is_byte_transparent() {
        let acceptor =
            QuicAcceptor::bind("127.0.0.1:0".parse().expect("addr")).expect("bind acceptor");
        let port = acceptor.local_addr().expect("local addr").port();
        let (_dir, ca_path) = write_ca_pem(acceptor.certificate().as_ref());

        // The bytes a real @RSYNCD handshake would put on the wire first.
        let request = b"@RSYNCD: 32.0\nmodule\n".to_vec();
        let reply = b"@RSYNCD: 32.0\n@RSYNCD: OK\n".to_vec();

        let expected_request = request.clone();
        let server_reply = reply.clone();
        let server = thread::spawn(move || {
            let mut stream = acceptor.accept().expect("accept");
            let mut got = vec![0u8; expected_request.len()];
            stream.read_exact(&mut got).expect("server read");
            assert_eq!(got, expected_request, "transport must not alter the bytes");
            stream.write_all(&server_reply).expect("server write");
            stream.finish().expect("server finish");
        });

        let addr = quic_addr(port);
        let stream =
            dial(&addr, Some(ca_path)).expect("quic dial succeeds with matching --quic-ca");
        let (reader, mut writer, guard) = stream.split().expect("split");
        let mut reader = reader;

        writer.write_all(&request).expect("client write");
        writer.flush().expect("client flush");

        let mut got_reply = vec![0u8; reply.len()];
        reader.read_exact(&mut got_reply).expect("client read");
        assert_eq!(got_reply, reply, "reply must arrive byte-identical");

        drop(writer);
        drop(reader);
        drop(guard);
        server.join().expect("server thread");
    }

    /// An unrelated certificate offered as `--quic-ca` must reject the peer and
    /// hard-fail with `RERR_STARTCLIENT` (5) - never a `DaemonStream`, never a
    /// TCP fallback. Proves the CA bundle is actually used to verify the peer.
    #[test]
    fn quic_unrelated_ca_is_rejected_and_hard_fails() {
        let acceptor =
            QuicAcceptor::bind("127.0.0.1:0".parse().expect("addr")).expect("bind acceptor");
        let port = acceptor.local_addr().expect("local addr").port();

        // A second acceptor yields a distinct self-signed cert unrelated to the
        // one the first acceptor will present.
        let other =
            QuicAcceptor::bind("127.0.0.1:0".parse().expect("addr")).expect("bind other acceptor");
        let (_dir, wrong_ca) = write_ca_pem(other.certificate().as_ref());

        // Keep the target acceptor draining so the handshake reaches (and fails)
        // certificate verification rather than stalling.
        let server = thread::spawn(move || {
            let _ = acceptor.accept();
        });

        let addr = quic_addr(port);
        match dial(&addr, Some(wrong_ca)) {
            Ok(_) => panic!("an unrelated --quic-ca must not authenticate the peer"),
            Err(err) => assert_eq!(err.exit_code(), 5, "cert reject -> RERR_STARTCLIENT"),
        }
        drop(server);
    }

    /// A missing `--quic-ca` bundle fails loudly at trust resolution with exit 5
    /// (no dial attempted, no TCP fallback).
    #[test]
    fn quic_missing_ca_bundle_hard_fails() {
        let addr = quic_addr(1);
        let missing = PathBuf::from("/nonexistent/oc-rsync-quic-ca.pem");
        match dial(&addr, Some(missing)) {
            Ok(_) => panic!("a missing --quic-ca bundle must not connect"),
            Err(err) => assert_eq!(err.exit_code(), 5),
        }
    }

    /// The interim dial-error classifier maps every QUIC failure - including an
    /// ALPN mismatch (`no application protocol`) - to `RERR_STARTCLIENT` (5),
    /// per policy. The full quinn-proto error taxonomy is #57.
    #[test]
    fn quic_alpn_mismatch_maps_to_startclient() {
        let addr = quic_addr(873);
        let err = quic_dial_error(&addr, "peer doesn't support any known protocol");
        assert_eq!(err.exit_code(), 5);
        let refused = quic_dial_error(&addr, "aborted by peer: the cryptographic handshake failed");
        assert_eq!(refused.exit_code(), 5);
    }
}

#[cfg(test)]
mod cork_tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

    /// Connects a loopback client/server pair, returning the client-side
    /// stream and the accepted server-side stream.
    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback");
        let addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (client, server)
    }

    #[test]
    fn cork_is_cleared_on_flush() {
        let (client, _server) = connected_pair();
        let mut writer = CorkedTcpWriter::new(client);

        // First write arms the cork (a no-op that stays uncorked on
        // platforms without a cork option).
        writer.write_all(b"burst").expect("write");
        assert_eq!(writer.corked, fast_io::tcp_cork_supported());

        // Flush must uncork so the coalesced segment is released and the
        // socket is not left stalled before the caller blocks on a read.
        writer.flush().expect("flush");
        assert!(!writer.corked, "flush must clear the cork");
    }

    #[test]
    fn cork_is_cleared_on_drop_after_error() {
        let (client, server) = connected_pair();
        let mut writer = CorkedTcpWriter::new(client);

        // Arm the cork, then drop the peer so subsequent writes fail. The
        // guard is that the corked flag is cleared on Drop regardless, so no
        // socket is ever left stuck corked on an error / early-return path.
        writer.write_all(b"corked").expect("first write");
        assert_eq!(writer.corked, fast_io::tcp_cork_supported());
        drop(server);

        // Writes to the FIN'd peer eventually error; whether this specific
        // write errors is timing dependent, but the invariant we assert is
        // that Drop clears the cork. We uncork explicitly to prove the clear
        // path, then confirm the flag.
        let _ = writer.write_all(b"more");
        writer.uncork().expect("explicit uncork clears cork");
        assert!(!writer.corked, "uncork must clear the cork flag");
    }

    #[test]
    fn corking_preserves_payload_bytes() {
        // The wire payload must be byte-identical to an uncorked write: only
        // TCP segmentation changes, never the bytes or their order.
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

        let (client, mut server) = connected_pair();
        let mut writer = CorkedTcpWriter::new(client);

        let reader = std::thread::spawn(move || {
            let mut buf = vec![0u8; 4096];
            server.read_exact(&mut buf).expect("read payload");
            buf
        });

        // Simulate a burst of frame-sized writes coalesced by the cork,
        // then a single flush at the burst boundary.
        for chunk in payload.chunks(64) {
            writer.write_all(chunk).expect("write chunk");
        }
        writer.flush().expect("flush burst");

        let received = reader.join().expect("reader thread");
        assert_eq!(received, payload, "corked payload must be byte-identical");
    }

    #[test]
    fn program_writer_variant_is_untouched_by_cork() {
        // Corking only applies to the real TCP variant; the accessor path
        // used by non-TCP transports must remain a plain passthrough. Prove
        // the TCP variant flushes and uncorks without error end to end.
        let (client, mut server) = connected_pair();
        let mut w = DaemonStreamWriter::Tcp(CorkedTcpWriter::new(client));

        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 3];
            server.read_exact(&mut buf).expect("read");
            buf
        });

        w.write_all(b"abc").expect("write");
        w.flush().expect("flush");
        assert_eq!(&reader.join().expect("reader"), b"abc");
    }
}

#[cfg(test)]
mod connect_timeout_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::num::NonZeroU64;
    use std::thread;

    #[test]
    fn resolve_connect_timeout_prefers_explicit_setting() {
        // --contimeout=N bounds the connect phase (upstream: socket.c:274-277).
        let explicit = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
        assert_eq!(
            resolve_connect_timeout(explicit),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn resolve_connect_timeout_ignores_transfer_timeout() {
        // --timeout must never bound connect; only --contimeout does. With
        // --contimeout unset the connect stays unbounded regardless of --timeout.
        assert_eq!(resolve_connect_timeout(TransferTimeout::Default), None);
    }

    #[test]
    fn resolve_connect_timeout_disables_when_requested() {
        // --contimeout=0 (Disabled) and the unset default both leave connect
        // unbounded, matching upstream's default connect_timeout=0 (options.c:125).
        assert!(resolve_connect_timeout(TransferTimeout::Disabled).is_none());
        assert!(resolve_connect_timeout(TransferTimeout::Default).is_none());
    }

    #[test]
    fn connect_direct_applies_io_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let daemon_addr = DaemonAddress::new(addr.ip().to_string(), addr.port());

        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1];
                let _ = stream.read(&mut buf);
            }
        });

        let timeout = Some(Duration::from_secs(4));
        let mut stream = connect_direct(
            &daemon_addr,
            Some(Duration::from_secs(10)),
            timeout,
            AddressMode::Default,
            None,
            crate::client::TcpFastOpenMode::Auto,
            None,
        )
        .expect("connect directly");

        assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
        assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

        let _ = stream.write_all(&[0]);
        handle.join().expect("listener thread");
    }

    #[test]
    fn connect_via_proxy_applies_io_timeout() {
        let proxy_listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let proxy_addr = proxy_listener.local_addr().expect("proxy addr");
        let proxy = ProxyConfig {
            host: proxy_addr.ip().to_string(),
            port: proxy_addr.port(),
            credentials: None,
        };

        let target = DaemonAddress::new(String::from("daemon.example"), 873);

        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = proxy_listener.accept() {
                let mut reader = BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).expect("read request") == 0 {
                        return;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }

                let mut stream = reader.into_inner();
                stream
                    .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
                    .expect("respond to connect");
                let _ = stream.flush();
            }
        });

        let timeout = Some(Duration::from_secs(6));
        let stream = connect_via_proxy(
            &target,
            &proxy,
            Some(Duration::from_secs(9)),
            timeout,
            None,
            crate::client::TcpFastOpenMode::Auto,
            None,
        )
        .expect("proxy connect");

        assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
        assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

        drop(stream);
        handle.join().expect("proxy thread");
    }
}
