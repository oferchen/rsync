//! The remote-shell data wire: the kernel primitive carrying the child's
//! stdin and stdout, and the parent-side reader and writer over it.
//!
//! upstream: pipe.c:57 `piped_child()` creates two `fd_pair()`s - one per
//! direction - and util1.c:84 `fd_pair()` is `socketpair(AF_UNIX,
//! SOCK_STREAM, 0)` wherever `HAVE_SOCKETPAIR` is defined. On Unix the wire
//! is therefore two socketpairs, exactly as upstream; if the kernel refuses
//! either pair the spawn falls back to `Stdio::piped()` anonymous pipes and
//! says so at `--debug=connect`. Windows has no `socketpair(2)` and always
//! uses pipes.
//!
//! The socketpair ends are real sockets, so on Linux the parent ends can be
//! driven by the `fast_io` io_uring `RECV`/`SEND` paths.

use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};

use logging::debug_log;

#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Kernel primitive carrying the remote-shell child's stdin and stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePrimitive {
    /// Two `AF_UNIX` `SOCK_STREAM` socketpairs, one per direction, as
    /// upstream `fd_pair()` creates them.
    Socketpair,
    /// Two anonymous pipes. The fallback when socketpair creation fails, and
    /// the only primitive on Windows.
    Pipe,
}

/// Parent end the client reads the child's stdout from.
#[cfg(unix)]
pub(super) type WireReadEnd = std::fs::File;
/// Parent end the client reads the child's stdout from.
#[cfg(not(unix))]
pub(super) type WireReadEnd = std::process::ChildStdout;

/// Parent end the client writes the child's stdin through.
#[cfg(unix)]
pub(super) type WireWriteEnd = std::fs::File;
/// Parent end the client writes the child's stdin through.
#[cfg(not(unix))]
pub(super) type WireWriteEnd = std::process::ChildStdin;

/// Spawn-time wiring decided before `Command::spawn`.
///
/// Holds the parent ends of the two socketpairs when they were installed, or
/// nothing when the command was configured with `Stdio::piped()` and the
/// parent ends will come from the spawned `Child`.
pub(super) struct WireSetup {
    /// `(to_child, from_child)` parent ends of the socketpairs.
    #[cfg(unix)]
    sockets: Option<(UnixStream, UnixStream)>,
}

/// Installs the child's stdin/stdout wire on `command`.
///
/// `child_stdout_blocking` is the resolved `--blocking-io` mode: upstream's
/// `fd_pair()` leaves every end non-blocking and pipe.c:80-82 then sets the
/// child's stdin blocking, and its stdout blocking only when `blocking_io`.
/// The parent ends stay blocking - unlike upstream, whose `io.c` multiplexes
/// non-blocking descriptors with `select()`, every reader and writer here
/// blocks (or waits on io_uring), so `O_NONBLOCK` would only turn waits into
/// `EAGAIN` errors.
pub(super) fn configure_wire(command: &mut Command, child_stdout_blocking: bool) -> WireSetup {
    #[cfg(unix)]
    {
        configure_wire_with(command, child_stdout_blocking, UnixStream::pair)
    }
    #[cfg(not(unix))]
    {
        let _ = child_stdout_blocking;
        install_pipes(command);
        debug_log!(
            Connect,
            2,
            "remote-shell wire: pipes (socketpair unavailable on this platform)"
        );
        WireSetup {}
    }
}

/// [`configure_wire`] with an injectable socketpair constructor, so tests
/// can force the pipe fallback.
#[cfg(unix)]
pub(super) fn configure_wire_with(
    command: &mut Command,
    child_stdout_blocking: bool,
    mut make_pair: impl FnMut() -> io::Result<(UnixStream, UnixStream)>,
) -> WireSetup {
    // upstream: pipe.c:57-60 - `fd_pair(to_child_pipe)` and
    // `fd_pair(from_child_pipe)`; either failing is fatal there. Here the
    // spawn degrades to pipes instead, which carry the same byte stream.
    let pairs = make_pair().and_then(|to_child| Ok((to_child, make_pair()?)));
    match pairs {
        Ok(((to_child_parent, to_child_child), (from_child_parent, from_child_child))) => {
            // upstream: pipe.c:80-82 - the child's stdin is blocking; its
            // stdout is blocking only under `blocking_io`.
            if !child_stdout_blocking && let Err(error) = from_child_child.set_nonblocking(true) {
                debug_log!(
                    Connect,
                    1,
                    "remote-shell wire: cannot make child stdout non-blocking ({error})"
                );
            }
            command.stdin(Stdio::from(OwnedFd::from(to_child_child)));
            command.stdout(Stdio::from(OwnedFd::from(from_child_child)));
            debug_log!(
                Connect,
                2,
                "remote-shell wire: socketpair (child stdout {})",
                if child_stdout_blocking {
                    "blocking"
                } else {
                    "non-blocking"
                }
            );
            WireSetup {
                sockets: Some((to_child_parent, from_child_parent)),
            }
        }
        Err(error) => {
            install_pipes(command);
            debug_log!(
                Connect,
                1,
                "remote-shell wire: socketpair unavailable ({error}); falling back to pipes"
            );
            WireSetup { sockets: None }
        }
    }
}

fn install_pipes(command: &mut Command) {
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
}

/// The parent's ends of a spawned child's wire.
#[derive(Debug)]
pub(super) struct WireEnds {
    /// Kernel primitive behind both ends.
    pub(super) primitive: WirePrimitive,
    /// Writes reach the child's stdin.
    pub(super) stdin: WireWriteEnd,
    /// Reads drain the child's stdout.
    pub(super) stdout: WireReadEnd,
}

impl WireSetup {
    /// Returns the parent ends once the child has been spawned.
    ///
    /// The child's own ends live in the `Command` that spawned it; the caller
    /// drops that `Command` so the parent keeps only these two descriptors
    /// and the child's exit is observable as EOF.
    pub(super) fn into_parent_ends(self, child: &mut Child) -> io::Result<WireEnds> {
        #[cfg(unix)]
        if let Some((to_child, from_child)) = self.sockets {
            return Ok(WireEnds {
                primitive: WirePrimitive::Socketpair,
                stdin: std::fs::File::from(OwnedFd::from(to_child)),
                stdout: std::fs::File::from(OwnedFd::from(from_child)),
            });
        }

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
        #[cfg(unix)]
        let (stdin, stdout) = (
            std::fs::File::from(OwnedFd::from(stdin)),
            std::fs::File::from(OwnedFd::from(stdout)),
        );
        Ok(WireEnds {
            primitive: WirePrimitive::Pipe,
            stdin,
            stdout,
        })
    }
}

/// io_uring and zero-copy preferences for the parent wire ends.
#[derive(Clone, Copy, Debug)]
pub(super) struct WireIoPolicy {
    /// Whether the socket ends are driven through io_uring `RECV`/`SEND`.
    pub(super) io_uring: fast_io::IoUringPolicy,
    /// Whether the send side asks for `IORING_OP_SEND_ZC`.
    pub(super) zero_copy: fast_io::ZeroCopyPolicy,
}

impl Default for WireIoPolicy {
    fn default() -> Self {
        Self {
            io_uring: fast_io::IoUringPolicy::Auto,
            zero_copy: fast_io::ZeroCopyPolicy::Auto,
        }
    }
}

/// io_uring receive buffer for the read end. Matches the 32 KiB `BufReader`
/// the transfer driver layers on top, whose full-buffer refills are then
/// large enough to bypass this reader's own buffer and land directly.
#[cfg(unix)]
const WIRE_READ_BUFFER: usize = 32 * 1024;

/// io_uring send buffer for the write end, sized to the multiplex frame
/// buffer so one frame flush is one submission.
#[cfg(unix)]
const WIRE_WRITE_BUFFER: usize = 64 * 1024;

/// Decides whether io_uring should drive a socketpair end.
///
/// Only an explicit `--io-uring` (`Enabled`) engages it: on the `AF_UNIX`
/// wire io_uring `RECV`/`SEND` measured no gain over plain `read`/`write`,
/// so `Auto`, `SqpollOff` and `Disabled` keep the plain syscalls. `Enabled`
/// on a host without io_uring (old kernel, seccomp,
/// `OC_RSYNC_DISABLE_IOURING`) is an error, matching the `fast_io` socket
/// factories.
#[cfg(unix)]
fn io_uring_wanted(policy: fast_io::IoUringPolicy) -> io::Result<bool> {
    match policy {
        fast_io::IoUringPolicy::Enabled if !fast_io::is_io_uring_available() => {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring requested but not available",
            ))
        }
        fast_io::IoUringPolicy::Enabled => Ok(true),
        _ => Ok(false),
    }
}

/// Parent read end of the wire, optionally driven by io_uring `RECV`.
pub(super) struct WireReader {
    // Declared before `end` so the io_uring reader, which borrows the raw fd,
    // is dropped before the descriptor is closed.
    #[cfg(unix)]
    uring: Option<fast_io::IoUringOrStdSocketReader>,
    end: WireReadEnd,
}

impl WireReader {
    /// Wraps `end`, engaging io_uring when the wire is a socketpair and
    /// `policy` allows it.
    pub(super) fn new(
        end: WireReadEnd,
        primitive: WirePrimitive,
        policy: WireIoPolicy,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            let mut uring = None;
            if primitive == WirePrimitive::Socketpair && io_uring_wanted(policy.io_uring)? {
                match fast_io::socket_reader_from_fd(
                    end.as_raw_fd(),
                    WIRE_READ_BUFFER,
                    policy.io_uring,
                )? {
                    reader @ fast_io::IoUringOrStdSocketReader::IoUring(_) => {
                        debug_log!(Connect, 2, "remote-shell wire: reading via io_uring RECV");
                        uring = Some(reader);
                    }
                    fast_io::IoUringOrStdSocketReader::Std(_) => {
                        debug_log!(
                            Connect,
                            2,
                            "remote-shell wire: io_uring ring setup failed; reading via read(2)"
                        );
                    }
                }
            }
            Ok(Self { uring, end })
        }
        #[cfg(not(unix))]
        {
            let _ = (primitive, policy);
            Ok(Self { end })
        }
    }

    /// Returns `true` when reads are submitted through io_uring.
    #[cfg(all(test, unix))]
    pub(super) fn uses_io_uring(&self) -> bool {
        self.uring.is_some()
    }
}

impl Read for WireReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        if let Some(uring) = self.uring.as_mut() {
            return uring.read(buf);
        }
        self.end.read(buf)
    }
}

impl std::fmt::Debug for WireReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("WireReader");
        #[cfg(unix)]
        debug.field("io_uring", &self.uring.is_some());
        debug.field("end", &self.end).finish()
    }
}

/// Parent write end of the wire, optionally driven by io_uring `SEND` (and
/// `SEND_ZC` where the socket accepts it).
///
/// Writes are write-through: every `write` reaches the kernel before it
/// returns, exactly like the unbuffered descriptor it replaces, so no caller
/// can block on the peer while bytes sit in a userspace buffer.
pub(super) struct WireWriter {
    // Declared before `end`; see `WireReader`.
    #[cfg(unix)]
    uring: Option<fast_io::IoUringOrStdSocketWriter>,
    end: WireWriteEnd,
}

impl WireWriter {
    /// Wraps `end`, engaging io_uring when the wire is a socketpair and
    /// `policy` allows it.
    ///
    /// When `policy.zero_copy` permits `IORING_OP_SEND_ZC`, the `fast_io`
    /// writer probes the socket once at construction; an `AF_UNIX` socket
    /// refuses with `EOPNOTSUPP` and the writer settles on plain `SEND`
    /// (traced at `--debug=iouring`), never failing a write over it.
    pub(super) fn new(
        end: WireWriteEnd,
        primitive: WirePrimitive,
        policy: WireIoPolicy,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            let mut uring = None;
            if primitive == WirePrimitive::Socketpair && io_uring_wanted(policy.io_uring)? {
                let fd = end.as_raw_fd();
                let zero_copy = fast_io::send_zc_policy_permits(policy.zero_copy);
                let writer = if zero_copy {
                    fast_io::socket_writer_from_fd_zero_copy(
                        fd,
                        WIRE_WRITE_BUFFER,
                        policy.zero_copy,
                    )?
                } else {
                    fast_io::socket_writer_from_fd(fd, WIRE_WRITE_BUFFER, policy.io_uring)?
                };
                match writer {
                    writer @ fast_io::IoUringOrStdSocketWriter::IoUring(_) => {
                        debug_log!(
                            Connect,
                            2,
                            "remote-shell wire: writing via io_uring SEND (zero-copy {})",
                            if zero_copy { "requested" } else { "off" }
                        );
                        uring = Some(writer);
                    }
                    fast_io::IoUringOrStdSocketWriter::Std(_) => {
                        debug_log!(
                            Connect,
                            2,
                            "remote-shell wire: io_uring ring setup failed; writing via write(2)"
                        );
                    }
                }
            }
            Ok(Self { uring, end })
        }
        #[cfg(not(unix))]
        {
            let _ = (primitive, policy);
            Ok(Self { end })
        }
    }

    /// Returns `true` when writes are submitted through io_uring.
    #[cfg(all(test, unix))]
    pub(super) fn uses_io_uring(&self) -> bool {
        self.uring.is_some()
    }
}

impl Write for WireWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        if let Some(uring) = self.uring.as_mut() {
            let n = uring.write(buf)?;
            uring.flush()?;
            return Ok(n);
        }
        self.end.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(uring) = self.uring.as_mut() {
            return uring.flush();
        }
        self.end.flush()
    }
}

impl std::fmt::Debug for WireWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("WireWriter");
        #[cfg(unix)]
        debug.field("io_uring", &self.uring.is_some());
        debug.field("end", &self.end).finish()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Compares two byte streams without dumping megabytes on failure.
    fn assert_same_stream(got: &[u8], want: &[u8]) {
        let first_diff = got.iter().zip(want).position(|(a, b)| a != b);
        assert_eq!(
            first_diff, None,
            "stream corrupted at the first differing offset"
        );
        assert_eq!(got.len(), want.len(), "stream length differs");
    }

    fn sh_command(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    fn spawn_with(
        mut command: Command,
        setup: impl FnOnce(&mut Command) -> WireSetup,
    ) -> (WirePrimitive, WireWriteEnd, WireReadEnd, Child) {
        let setup = setup(&mut command);
        let mut child = command.spawn().expect("spawn sh");
        drop(command);
        let ends = setup.into_parent_ends(&mut child).expect("parent ends");
        (ends.primitive, ends.stdin, ends.stdout, child)
    }

    fn echo_round_trip(stdin: WireWriteEnd, stdout: WireReadEnd, primitive: WirePrimitive) {
        let mut writer = WireWriter::new(stdin, primitive, WireIoPolicy::default()).unwrap();
        let mut reader = WireReader::new(stdout, primitive, WireIoPolicy::default()).unwrap();
        let payload: Vec<u8> = (0..256 * 1024u32).map(|i| (i % 253) as u8).collect();
        let expected = payload.clone();
        let feeder = std::thread::spawn(move || {
            writer.write_all(&payload).unwrap();
            drop(writer);
        });
        let mut echoed = Vec::new();
        reader.read_to_end(&mut echoed).unwrap();
        feeder.join().unwrap();
        assert_same_stream(&echoed, &expected);
    }

    /// Mirrors upstream `fd_pair()`: with the kernel granting socketpairs the
    /// wire is two of them, and the bytes round-trip through the child.
    ///
    /// `cat` does not retry `EAGAIN` on a non-blocking stdout, so it runs with
    /// blocking I/O - what upstream's `--blocking-io` exists for.
    #[test]
    fn wire_prefers_socketpairs() {
        let (primitive, stdin, stdout, mut child) =
            spawn_with(sh_command("exec cat"), |c| configure_wire(c, true));
        assert_eq!(primitive, WirePrimitive::Socketpair);
        assert!(std::os::unix::fs::FileTypeExt::is_socket(
            &stdin.metadata().unwrap().file_type()
        ));
        echo_round_trip(stdin, stdout, primitive);
        assert!(child.wait().unwrap().success());
    }

    /// A kernel that refuses `socketpair(2)` must still yield a working wire
    /// over pipes, and the selection must say so.
    #[test]
    fn wire_falls_back_to_pipes_when_socketpair_fails() {
        let refuse = || -> io::Result<(UnixStream, UnixStream)> {
            Err(io::Error::from_raw_os_error(24)) // EMFILE
        };
        let (primitive, stdin, stdout, mut child) = spawn_with(sh_command("exec cat"), |c| {
            configure_wire_with(c, false, refuse)
        });
        assert_eq!(primitive, WirePrimitive::Pipe);
        assert!(std::os::unix::fs::FileTypeExt::is_fifo(
            &stdin.metadata().unwrap().file_type()
        ));
        echo_round_trip(stdin, stdout, primitive);
        assert!(child.wait().unwrap().success());
    }

    /// If only the second pair fails, the first must not leak into the
    /// command: both directions fall back together.
    #[test]
    fn wire_falls_back_when_only_the_second_pair_fails() {
        let mut calls = 0;
        let second_fails = || {
            calls += 1;
            if calls == 1 {
                UnixStream::pair()
            } else {
                Err(io::Error::from_raw_os_error(23)) // ENFILE
            }
        };
        let (primitive, stdin, stdout, mut child) = spawn_with(sh_command("exec cat"), |c| {
            configure_wire_with(c, false, second_fails)
        });
        assert_eq!(primitive, WirePrimitive::Pipe);
        echo_round_trip(stdin, stdout, primitive);
        assert!(child.wait().unwrap().success());
    }

    /// The child sees sockets on fds 0 and 1 - what upstream's `piped_child()`
    /// hands `ssh` - and stdout is non-blocking unless `--blocking-io`.
    #[cfg(target_os = "linux")]
    #[test]
    fn child_stdio_are_sockets_with_upstream_blocking_modes() {
        // The flags go through `$(...)` and are printed afterwards: dash applies
        // a command's redirections in the shell itself before forking, so
        // `grep ... >&2` would read the shell's fd 1 while it is stderr.
        let script = "for fd in 0 1; do [ -S /proc/self/fd/$fd ] && printf 'sock%s ' $fd; done; \
                      flags=$(grep -h '^flags' /proc/$$/fdinfo/0 /proc/$$/fdinfo/1); \
                      printf '%s\\n' \"$flags\" >&2";
        for blocking in [false, true] {
            let mut command = sh_command(script);
            command.stderr(Stdio::piped());
            let (primitive, stdin, mut stdout, child) =
                spawn_with(command, |c| configure_wire(c, blocking));
            assert_eq!(primitive, WirePrimitive::Socketpair);
            drop(stdin);
            let mut seen = String::new();
            stdout.read_to_string(&mut seen).unwrap();
            let output = child.wait_with_output().unwrap();
            assert_eq!(seen, "sock0 sock1 ");
            // fdinfo `flags` is octal; O_NONBLOCK is 04000.
            let flags: Vec<u32> = String::from_utf8(output.stderr)
                .unwrap()
                .lines()
                .map(|l| u32::from_str_radix(l.split_whitespace().nth(1).unwrap(), 8).unwrap())
                .collect();
            assert_eq!(flags.len(), 2);
            assert_eq!(flags[0] & 0o4000, 0, "child stdin must be blocking");
            assert_eq!(
                flags[1] & 0o4000 != 0,
                !blocking,
                "child stdout non-blocking iff blocking_io is off"
            );
        }
    }

    /// The default wire is plain `read`/`write` even where the host has
    /// io_uring: on `AF_UNIX` io_uring measured no gain, so it must not engage
    /// without an explicit `--io-uring`.
    #[test]
    fn default_wire_uses_plain_read_write() {
        let (a, b) = UnixStream::pair().unwrap();
        let writer = WireWriter::new(
            std::fs::File::from(OwnedFd::from(a)),
            WirePrimitive::Socketpair,
            WireIoPolicy::default(),
        )
        .unwrap();
        let reader = WireReader::new(
            std::fs::File::from(OwnedFd::from(b)),
            WirePrimitive::Socketpair,
            WireIoPolicy::default(),
        )
        .unwrap();
        assert!(
            !writer.uses_io_uring(),
            "default writer must not use io_uring"
        );
        assert!(
            !reader.uses_io_uring(),
            "default reader must not use io_uring"
        );
    }

    /// An explicit `--io-uring` engages io_uring on socketpair ends where the
    /// host has it, never on pipes, and never when the policy disables it.
    #[test]
    fn io_uring_engages_only_on_explicit_request_and_socketpair_ends() {
        if !fast_io::is_io_uring_available() {
            eprintln!("skipping: io_uring unavailable on this host");
            return;
        }
        let (a, b) = UnixStream::pair().unwrap();
        let (a, b) = (
            std::fs::File::from(OwnedFd::from(a)),
            std::fs::File::from(OwnedFd::from(b)),
        );
        let (c, d) = (a.try_clone().unwrap(), b.try_clone().unwrap());
        let requested = WireIoPolicy {
            io_uring: fast_io::IoUringPolicy::Enabled,
            zero_copy: fast_io::ZeroCopyPolicy::Auto,
        };

        let writer = WireWriter::new(a, WirePrimitive::Socketpair, requested).unwrap();
        let reader = WireReader::new(b, WirePrimitive::Socketpair, requested).unwrap();
        assert!(writer.uses_io_uring());
        assert!(reader.uses_io_uring());

        let disabled = WireIoPolicy {
            io_uring: fast_io::IoUringPolicy::Disabled,
            zero_copy: fast_io::ZeroCopyPolicy::Enabled,
        };
        assert!(
            !WireWriter::new(c, WirePrimitive::Socketpair, disabled)
                .unwrap()
                .uses_io_uring()
        );
        assert!(
            !WireReader::new(d, WirePrimitive::Pipe, requested)
                .unwrap()
                .uses_io_uring()
        );
    }

    /// Zero-copy requested over the io_uring `AF_UNIX` wire: the socket
    /// refuses `SEND_ZC`, and the writer must still deliver every byte.
    #[test]
    fn zero_copy_request_over_socketpair_delivers_bytes() {
        if !fast_io::is_io_uring_available() {
            eprintln!("skipping: io_uring unavailable on this host");
            return;
        }
        let (a, b) = UnixStream::pair().unwrap();
        let policy = WireIoPolicy {
            io_uring: fast_io::IoUringPolicy::Enabled,
            zero_copy: fast_io::ZeroCopyPolicy::Enabled,
        };
        let mut writer = WireWriter::new(
            std::fs::File::from(OwnedFd::from(a)),
            WirePrimitive::Socketpair,
            policy,
        )
        .unwrap();
        let mut reader = WireReader::new(
            std::fs::File::from(OwnedFd::from(b)),
            WirePrimitive::Socketpair,
            policy,
        )
        .unwrap();
        let payload: Vec<u8> = (0..1024 * 1024u32).map(|i| (i % 241) as u8).collect();
        let expected = payload.clone();
        let feeder = std::thread::spawn(move || {
            writer.write_all(&payload).unwrap();
        });
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        feeder.join().unwrap();
        assert_same_stream(&received, &expected);
    }
}
