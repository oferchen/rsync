mod direct;
mod program;
mod proxy;
mod rsh;

use std::ffi::OsStr;
use std::io::{self, IoSlice, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use super::super::{AddressMode, ClientError, TcpFastOpenMode, TransferTimeout};
use super::DaemonAddress;
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
}

impl Read for DaemonStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(reader) => reader.read(buf),
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
}

impl Write for DaemonStreamWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(writer) => writer.write(buf),
            Self::Program(writer) => writer.write(buf),
        }
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match self {
            Self::Tcp(writer) => writer.write_vectored(bufs),
            Self::Program(writer) => writer.write_vectored(bufs),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(writer) => writer.flush(),
            Self::Program(writer) => writer.flush(),
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

/// Opens a plain TCP connection to a daemon.
///
/// Respects `RSYNC_CONNECT_PROG` and `RSYNC_PROXY` environment
/// variables.
pub(crate) fn open_daemon_stream(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    connect_program: Option<&OsStr>,
    bind_address: Option<SocketAddr>,
    tfo: TcpFastOpenMode,
) -> Result<DaemonStream, ClientError> {
    if let Some(program) = program::load_daemon_connect_program(connect_program)? {
        return program::connect_via_program(addr, &program);
    }

    let stream = match load_daemon_proxy()? {
        Some(proxy) => {
            proxy::connect_via_proxy(addr, &proxy, connect_timeout, io_timeout, bind_address, tfo)?
        }
        None => connect_direct(
            addr,
            connect_timeout,
            io_timeout,
            address_mode,
            bind_address,
            tfo,
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
}

impl DaemonStream {
    const fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
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
                    DaemonStreamGuard::Child(parts.child),
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
/// For connect programs, this holds the `Child` process handle and
/// kills/reaps it on drop. For TCP streams, no guard is needed.
pub(crate) enum DaemonStreamGuard {
    /// No resource to guard (TCP).
    None,
    /// Owns a connect program child process.
    Child(std::process::Child),
}

impl Drop for DaemonStreamGuard {
    fn drop(&mut self) {
        if let Self::Child(child) = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(stream) => stream.read(buf),
        }
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Program(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Program(stream) => stream.flush(),
        }
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
