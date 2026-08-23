//! Connect program support for daemon connections via `RSYNC_CONNECT_PROG`.
//!
//! This module provides [`ConnectProgramConfig`] for executing custom connection
//! programs, mirroring upstream rsync's `RSYNC_CONNECT_PROG` functionality.
//!
//! # Loopback TCP Socketpair Transport
//!
//! On Unix, the child process's stdin and stdout are connected via a loopback
//! TCP socketpair (AF_INET on `127.0.0.1`) instead of OS pipes, mirroring
//! upstream rsync's `sock_exec()` in `socket.c:811-841`, which uses
//! `socketpair_tcp()`. A socket (not a pipe) is required so the child's
//! `STDIN_FILENO` passes the `getsockopt(SO_TYPE)` check in `is_a_socket()`
//! (`socket.c:500`). The AF_INET loopback family (rather than an AF_UNIX
//! socketpair) additionally gives a daemon reached this way a real `127.0.0.1`
//! peer address via `getpeername()`, which it needs for `hosts allow` /
//! `hosts deny` matching - an AF_UNIX peer carries no address and is rejected
//! as `UNKNOWN`.
//!
//! On non-Unix platforms, standard pipes are used since inetd-style
//! detection does not apply.
//!
//! # Security Model
//!
//! The rendered command is run as `sh -c <command>`, so `%H` is substituted
//! into a word a shell parses. The template is operator-supplied, but the host
//! is not: it comes from the `rsync://host/module` operand. Upstream therefore
//! applies two defences before substituting, and so does this module
//! (upstream: socket.c:488-495):
//!
//! - the host is refused outright unless every byte stays data through a shell
//!   pass, and its first byte is additionally restricted
//!   ([`shell_unsafe_connect_host`]);
//! - what remains is single-quoted so the outer shell sees one literal word
//!   ([`shell_quote_connect_host`]).
//!
//! Both are needed. Quoting alone cannot help when a hook of the form
//! `sh -c '... %H ...'` re-parses the word in a nested shell, where the quotes
//! have already been consumed by the outer one.
//!
//! This bounds what a *shell* can do with the value. It cannot bound what the
//! named program does with it - `host:-rf` arrives intact, and a program that
//! splits on `:` may reinterpret the tail. That boundary belongs to whoever
//! writes `RSYNC_CONNECT_PROG`.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use super::super::DaemonAddress;
use crate::client::{
    ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, SOCKET_IO_EXIT_CODE, daemon_error,
};

/// Spawns a connect program with a loopback TCP socketpair for stdin/stdout.
///
/// Uses [`socketpair_tcp`] (an AF_INET pair on `127.0.0.1`) so the child's
/// stdin fd is a socket, not a pipe - satisfying the daemon's
/// `is_a_socket(STDIN_FILENO)` inetd check while also presenting a real
/// `127.0.0.1` peer address for the daemon's `hosts allow` / `hosts deny`
/// matching.
///
/// upstream: socket.c:811-841 - `sock_exec()` uses `socketpair_tcp()` and
/// `dup2(fd[1], STDIN_FILENO)` / `dup2(fd[1], STDOUT_FILENO)` in the child.
#[cfg(unix)]
pub(crate) fn connect_via_program(
    addr: &DaemonAddress,
    program: &ConnectProgramConfig,
) -> Result<super::DaemonStream, ClientError> {
    let command = program
        .format_command(addr.host(), addr.port())
        .map_err(refused_connect_host_error)?;

    let shell = program
        .shell()
        .cloned()
        .unwrap_or_else(|| OsString::from("sh"));

    // upstream: socket.c:816 sock_exec() -> socket.c:744 socketpair_tcp(fd).
    // A connected AF_INET pair on the loopback address, NOT a Unix-domain
    // socketpair: both ends carry a real 127.0.0.1 peer address, so a daemon
    // that derives the client address from getpeername() for `hosts allow` /
    // `hosts deny` matching sees a valid IP instead of an unnamed AF_UNIX
    // socket (which has no address and is rejected as "UNKNOWN").
    let (parent_sock, child_sock) = socketpair_tcp().map_err(|error| {
        daemon_error(
            format!("failed to create loopback socketpair for RSYNC_CONNECT_PROG: {error}"),
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    // The child gets one end of the socketpair as both stdin and stdout.
    // upstream: socket.c:831-832 - dup2(fd[1], STDIN_FILENO), dup2(fd[1], STDOUT_FILENO)
    //
    // We need two `Stdio` values from the same fd. `Stdio::from()` consumes
    // the `OwnedFd`, so clone the child socket for the second handle.
    let child_sock_dup = child_sock.try_clone().map_err(|error| {
        daemon_error(
            format!("failed to dup socketpair fd for RSYNC_CONNECT_PROG: {error}"),
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let child_stdin = Stdio::from(std::os::fd::OwnedFd::from(child_sock));
    let child_stdout = Stdio::from(std::os::fd::OwnedFd::from(child_sock_dup));

    let mut builder = Command::new(&shell);
    builder.arg("-c").arg(&command);
    builder.stdin(child_stdin);
    builder.stdout(child_stdout);
    builder.stderr(Stdio::inherit());
    builder.env("RSYNC_PORT", addr.port().to_string());

    let child = builder.spawn().map_err(|error| {
        daemon_error(
            format!(
                "failed to spawn RSYNC_CONNECT_PROG using shell '{}': {error}",
                Path::new(&shell).display()
            ),
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    // Parent keeps fd[0] for reading and writing.
    // upstream: socket.c:839-840 - close(fd[1]); return fd[0];
    Ok(super::DaemonStream::program(
        ConnectProgramStream::from_socketpair(child, parent_sock),
    ))
}

/// Creates a connected AF_INET socket pair on the loopback address.
///
/// std equivalent of upstream rsync's `socketpair_tcp()` (`socket.c:744`):
/// bind a listener on `127.0.0.1:0`, connect a second socket to it, and accept.
/// Returns `(accepted, connected)` - two TCP streams that are peers of each
/// other, both reporting a `127.0.0.1` peer address via `getpeername()`.
///
/// This is used in place of `UnixStream::pair()` so a connect-program daemon
/// can derive the client address for `hosts allow` / `hosts deny` matching: an
/// AF_UNIX socketpair has no peer address and is rejected as `UNKNOWN`, whereas
/// the loopback pair presents `127.0.0.1`. The accepted/connected fds are
/// ordinary blocking sockets, so no further setup is required (std's
/// `TcpStream::connect` completes the handshake synchronously, unlike the
/// non-blocking dance upstream performs in C).
#[cfg(unix)]
fn socketpair_tcp() -> io::Result<(std::net::TcpStream, std::net::TcpStream)> {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let addr = listener.local_addr()?;
    let connected = TcpStream::connect(addr)?;
    let (accepted, _peer) = listener.accept()?;
    Ok((accepted, connected))
}

/// Spawns a connect program with piped stdin/stdout (non-Unix fallback).
///
/// On non-Unix platforms, inetd-style socket detection does not apply, so
/// standard pipes are acceptable.
#[cfg(not(unix))]
pub(crate) fn connect_via_program(
    addr: &DaemonAddress,
    program: &ConnectProgramConfig,
) -> Result<super::DaemonStream, ClientError> {
    let command = program
        .format_command(addr.host(), addr.port())
        .map_err(refused_connect_host_error)?;

    let shell = program
        .shell()
        .cloned()
        .unwrap_or_else(|| OsString::from("sh"));

    let mut builder = Command::new(&shell);
    builder.arg("-c").arg(&command);
    builder.stdin(Stdio::piped());
    builder.stdout(Stdio::piped());
    builder.stderr(Stdio::inherit());
    builder.env("RSYNC_PORT", addr.port().to_string());

    let mut child = builder.spawn().map_err(|error| {
        daemon_error(
            format!(
                "failed to spawn RSYNC_CONNECT_PROG using shell '{}': {error}",
                Path::new(&shell).display()
            ),
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdin = child.stdin.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a writable stdin",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a readable stdout",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    Ok(super::DaemonStream::program(
        ConnectProgramStream::from_pipes(child, stdin, stdout),
    ))
}

pub(crate) fn load_daemon_connect_program(
    override_template: Option<&OsStr>,
) -> Result<Option<ConnectProgramConfig>, ClientError> {
    if let Some(template) = override_template {
        if template.is_empty() {
            return Err(connect_program_configuration_error(
                "the --connect-program option requires a non-empty command",
            ));
        }

        let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());
        return ConnectProgramConfig::new(OsString::from(template), shell)
            .map(Some)
            .map_err(connect_program_configuration_error);
    }

    let Some(template) = env::var_os("RSYNC_CONNECT_PROG") else {
        return Ok(None);
    };

    if template.is_empty() {
        return Err(connect_program_configuration_error(
            "RSYNC_CONNECT_PROG must not be empty",
        ));
    }

    let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());

    ConnectProgramConfig::new(template, shell)
        .map(Some)
        .map_err(connect_program_configuration_error)
}

#[derive(Debug)]
pub(crate) struct ConnectProgramConfig {
    template: OsString,
    shell: Option<OsString>,
}

impl ConnectProgramConfig {
    pub(crate) fn new(template: OsString, shell: Option<OsString>) -> Result<Self, String> {
        if template.is_empty() {
            return Err("RSYNC_CONNECT_PROG must not be empty".to_owned());
        }

        if shell.as_ref().is_some_and(|value| value.is_empty()) {
            return Err("RSYNC_SHELL must not be empty".to_owned());
        }

        Ok(Self { template, shell })
    }

    pub(crate) const fn shell(&self) -> Option<&OsString> {
        self.shell.as_ref()
    }

    /// Whether the template contains a substitution specifier at all.
    ///
    /// upstream: socket.c:488 - `strchr(prog, '%')`. A `%` byte is ASCII, so a
    /// lossy view answers this question identically for a non-UTF-8 template.
    fn template_has_specifier(&self) -> bool {
        self.template.to_string_lossy().contains('%')
    }

    /// Renders the template, substituting `%H` and `%P`.
    ///
    /// Returns [`UNSAFE_CONNECT_HOST`] when the host cannot be safely
    /// substituted; the rendered command is run through `sh -c`, so an
    /// unchecked host would be shell syntax rather than data.
    pub(crate) fn format_command(&self, host: &str, port: u16) -> Result<OsString, String> {
        // upstream: socket.c:488-495 - the refusal, the quoting and the
        // substitution all sit inside `if (prog && strchr(prog, '%'))`, so a
        // template with no specifier never inspects the host at all.
        if !self.template_has_specifier() {
            return Ok(self.template.clone());
        }
        if shell_unsafe_connect_host(host) {
            return Err(UNSAFE_CONNECT_HOST.to_owned());
        }
        let quoted = shell_quote_connect_host(host);
        let host = quoted.as_str();

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let template = self.template.as_os_str().as_bytes();
            let mut rendered = Vec::with_capacity(template.len() + host.len());
            let mut iter = template.iter().copied();
            let host_bytes = host.as_bytes();
            let port_string = port.to_string();
            let port_bytes = port_string.as_bytes();

            while let Some(byte) = iter.next() {
                if byte == b'%' {
                    match iter.next() {
                        Some(b'%') => rendered.push(b'%'),
                        Some(b'H') => rendered.extend_from_slice(host_bytes),
                        Some(b'P') => rendered.extend_from_slice(port_bytes),
                        Some(other) => {
                            rendered.push(b'%');
                            rendered.push(other);
                        }
                        None => rendered.push(b'%'),
                    }
                } else {
                    rendered.push(byte);
                }
            }

            Ok(OsString::from_vec(rendered))
        }

        #[cfg(not(unix))]
        {
            let template = self.template.as_os_str().to_string_lossy();
            let mut rendered = String::with_capacity(template.len() + host.len());
            let mut chars = template.chars();
            let port_string = port.to_string();

            while let Some(ch) = chars.next() {
                if ch == '%' {
                    match chars.next() {
                        Some('%') => rendered.push('%'),
                        Some('H') => rendered.push_str(host),
                        Some('P') => rendered.push_str(&port_string),
                        Some(other) => {
                            rendered.push('%');
                            rendered.push(other);
                        }
                        None => rendered.push('%'),
                    }
                } else {
                    rendered.push(ch);
                }
            }

            Ok(OsString::from(rendered))
        }
    }
}

/// Bidirectional stream to a connect program child process.
///
/// On Unix, the transport is a Unix domain socketpair so that the child's
/// stdin fd passes `getsockopt(SO_TYPE)` (inetd detection). On non-Unix,
/// standard pipes are used.
pub(crate) struct ConnectProgramStream {
    /// `None` after `into_parts()` has been called.
    child: Option<Child>,
    /// Parent end of the socketpair (Unix) or pipe handles (non-Unix).
    /// `None` after `into_parts()` has been called.
    transport: Option<ProgramTransport>,
}

/// Platform-specific transport backing a connect program stream.
///
/// On Unix, a single `UnixStream` serves as both the read and write channel
/// (mirroring upstream rsync's `sock_exec()` which returns a single fd).
/// On non-Unix, separate pipe handles are used.
#[cfg(unix)]
enum ProgramTransport {
    Socket(std::net::TcpStream),
    Pipe {
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
    },
}

#[cfg(not(unix))]
struct ProgramTransport {
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
}

impl ConnectProgramStream {
    /// Creates a stream backed by a loopback TCP socketpair.
    #[cfg(unix)]
    fn from_socketpair(child: Child, parent_socket: std::net::TcpStream) -> Self {
        Self {
            child: Some(child),
            transport: Some(ProgramTransport::Socket(parent_socket)),
        }
    }

    /// Creates a stream backed by OS pipes.
    ///
    /// On non-Unix this is the only transport. On Unix this is used for
    /// daemon-over-remote-shell where SSH pipes do not need the socketpair
    /// trick required by `RSYNC_CONNECT_PROG` inetd detection.
    pub(super) fn from_pipes(
        child: Child,
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
    ) -> Self {
        #[cfg(unix)]
        let transport = ProgramTransport::Pipe { stdin, stdout };
        #[cfg(not(unix))]
        let transport = ProgramTransport { stdin, stdout };

        Self {
            child: Some(child),
            transport: Some(transport),
        }
    }

    /// Decomposes the stream into its constituent parts for split I/O.
    ///
    /// On Unix, returns `(child, reader_socket, writer_socket)` where both
    /// sockets are handles to the same underlying socketpair fd (via
    /// `try_clone`). On non-Unix, returns `(child, stdout, stdin)`.
    pub(super) fn into_parts(mut self) -> io::Result<ConnectProgramParts> {
        let child = self.child.take().expect("child already taken");
        let transport = self.transport.take().expect("transport already taken");

        #[cfg(unix)]
        {
            match transport {
                ProgramTransport::Socket(socket) => match socket.try_clone() {
                    Ok(writer) => Ok(ConnectProgramParts {
                        child,
                        reader: ProgramReader::Socket(socket),
                        writer: ProgramWriter::Socket(writer),
                    }),
                    Err(e) => {
                        let mut child = child;
                        let _ = child.wait();
                        Err(e)
                    }
                },
                ProgramTransport::Pipe { stdin, stdout } => Ok(ConnectProgramParts {
                    child,
                    reader: ProgramReader::Pipe(stdout),
                    writer: ProgramWriter::Pipe(stdin),
                }),
            }
        }

        #[cfg(not(unix))]
        {
            Ok(ConnectProgramParts {
                child,
                reader: transport.stdout,
                writer: transport.stdin,
            })
        }
    }
}

/// Decomposed parts of a [`ConnectProgramStream`] for split read/write.
pub(super) struct ConnectProgramParts {
    /// The child process handle.
    pub(super) child: Child,
    /// Read half: Unix socketpair clone, child stdout pipe, or equivalent.
    #[cfg(unix)]
    pub(super) reader: ProgramReader,
    #[cfg(not(unix))]
    pub(super) reader: std::process::ChildStdout,
    /// Write half: Unix socketpair clone, child stdin pipe, or equivalent.
    #[cfg(unix)]
    pub(super) writer: ProgramWriter,
    #[cfg(not(unix))]
    pub(super) writer: std::process::ChildStdin,
}

/// Read half of a connect program stream on Unix.
///
/// Either a cloned socketpair end (for `RSYNC_CONNECT_PROG`) or a child
/// stdout pipe (for daemon-over-remote-shell).
#[cfg(unix)]
pub(in crate::client) enum ProgramReader {
    Socket(std::net::TcpStream),
    Pipe(std::process::ChildStdout),
}

#[cfg(unix)]
impl Read for ProgramReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Socket(s) => s.read(buf),
            Self::Pipe(p) => p.read(buf),
        }
    }
}

/// Write half of a connect program stream on Unix.
///
/// Either a cloned socketpair end (for `RSYNC_CONNECT_PROG`) or a child
/// stdin pipe (for daemon-over-remote-shell).
#[cfg(unix)]
pub(in crate::client) enum ProgramWriter {
    Socket(std::net::TcpStream),
    Pipe(std::process::ChildStdin),
}

#[cfg(unix)]
impl Write for ProgramWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Socket(s) => s.write(buf),
            Self::Pipe(p) => p.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Socket(s) => s.flush(),
            Self::Pipe(p) => p.flush(),
        }
    }
}

impl Read for ConnectProgramStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let transport = self
            .transport
            .as_mut()
            .expect("transport taken by into_parts");
        #[cfg(unix)]
        {
            match transport {
                ProgramTransport::Socket(socket) => socket.read(buf),
                ProgramTransport::Pipe { stdout, .. } => stdout.read(buf),
            }
        }
        #[cfg(not(unix))]
        {
            transport.stdout.read(buf)
        }
    }
}

impl Write for ConnectProgramStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let transport = self
            .transport
            .as_mut()
            .expect("transport taken by into_parts");
        #[cfg(unix)]
        {
            match transport {
                ProgramTransport::Socket(socket) => socket.write(buf),
                ProgramTransport::Pipe { stdin, .. } => stdin.write(buf),
            }
        }
        #[cfg(not(unix))]
        {
            transport.stdin.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let transport = self
            .transport
            .as_mut()
            .expect("transport taken by into_parts");
        #[cfg(unix)]
        {
            match transport {
                ProgramTransport::Socket(socket) => socket.flush(),
                ProgramTransport::Pipe { stdin, .. } => stdin.flush(),
            }
        }
        #[cfg(not(unix))]
        {
            transport.stdin.flush()
        }
    }
}

impl Drop for ConnectProgramStream {
    fn drop(&mut self) {
        // Close our end first so the child sees EOF and exits on its own, then
        // reap it. The child is never signalled: whatever it still has to do
        // after the last byte - a daemon spawned through `RSYNC_CONNECT_PROG`
        // runs its `post-xfer exec` hook there - must be allowed to finish.
        //
        // upstream: socket.c:1046 `sock_exec()` forks the connect program and
        // never signals it; the child ends when the socket closes.
        drop(self.transport.take());
        if let Some(child) = &mut self.child {
            let _ = child.wait();
        }
    }
}

/// The refusal text upstream prints when a host cannot be substituted.
///
/// upstream: socket.c:492 - `rprintf(FERROR, "unsafe host characters for
/// RSYNC_CONNECT_PROG\n")`.
const UNSAFE_CONNECT_HOST: &str = "unsafe host characters for RSYNC_CONNECT_PROG";

/// Whether `byte` stays data when a shell re-parses the word containing it.
///
/// upstream: socket.c:209-212. The set is deliberately wider than a DNS name:
/// `RSYNC_CONNECT_PROG` exists for custom transports, where `%H` may be an
/// alias the program resolves itself rather than a name the resolver ever sees,
/// so `+` and `~` are allowed inside the string. `%` is required for an IPv6
/// zone id (`fe80::1%eth0`) and is not special to a POSIX shell.
const fn connect_host_byte_is_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-' | b'%' | b'+' | b'~')
}

/// Whether `host` must be refused rather than substituted into the template.
///
/// upstream: socket.c:204-215 `shell_unsafe_connect_host()`.
///
/// The first byte is checked separately because several of the otherwise
/// allowed ones change an argument's MEANING rather than its text, which no
/// amount of quoting prevents. Mid-word each is literal, which is why the set
/// still allows them:
/// - `-` and `+` both introduce options to plenty of programs - `sh +x` is as
///   real as `sh -x`;
/// - `~` is tilde-expanded by a nested shell, turning `~root` into `/root`;
/// - `%` is expanded by a nested fish, where `%self` becomes its pid (an IPv6
///   zone id has its `%` mid-word, so nothing legitimate is lost).
///
/// An empty host is refused too: it survives a direct exec as an empty
/// argument but vanishes entirely when a nested shell re-splits the command,
/// shifting every argument after it.
fn shell_unsafe_connect_host(host: &str) -> bool {
    match host.as_bytes() {
        [] => true,
        [b'-' | b'+' | b'~' | b'%', ..] => true,
        bytes => !bytes.iter().copied().all(connect_host_byte_is_safe),
    }
}

/// Wraps `host` in single quotes so the outer shell sees exactly one literal
/// word, rendering an embedded quote as `'\''`.
///
/// upstream: socket.c `shell_quote_connect_host()`.
fn shell_quote_connect_host(host: &str) -> String {
    let mut quoted = String::with_capacity(host.len() + 2);
    quoted.push('\'');
    for ch in host.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// Maps a [`ConnectProgramConfig::format_command`] refusal onto upstream's exit
/// code for it.
///
/// `format_command` fails for exactly one reason - the host did not survive
/// [`shell_unsafe_connect_host`] - and upstream reports that as a failure to
/// open the connection, not as a usage error: `open_socket_out_wrapped()`
/// returns -1 (socket.c:490-493) and `start_socket_client()` answers with
/// `exit_cleanup(RERR_SOCKETIO)` (clientserver.c:163-165).
///
/// Both `connect_via_program` arms route through here so the unix and non-unix
/// builds cannot drift to different codes.
fn refused_connect_host_error(error: String) -> ClientError {
    daemon_error(error, SOCKET_IO_EXIT_CODE)
}

fn connect_program_configuration_error(text: impl Into<String>) -> ClientError {
    let message = crate::rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, "{}", text.into())
        .with_role(crate::message::Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_program_config_new_valid() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None);
        assert!(config.is_ok());
    }

    #[test]
    fn connect_program_config_new_with_shell() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), Some("/bin/bash".into()));
        assert!(config.is_ok());
        assert!(config.unwrap().shell().is_some());
    }

    #[test]
    fn connect_program_config_new_empty_template_error() {
        let config = ConnectProgramConfig::new("".into(), None);
        assert!(config.is_err());
        assert!(config.unwrap_err().contains("empty"));
    }

    #[test]
    fn connect_program_config_new_empty_shell_error() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), Some("".into()));
        assert!(config.is_err());
        assert!(config.unwrap_err().contains("RSYNC_SHELL"));
    }

    #[test]
    fn connect_program_config_shell_none() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        assert!(config.shell().is_none());
    }

    #[test]
    fn connect_program_config_shell_some() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), Some("/bin/zsh".into())).unwrap();
        assert_eq!(config.shell().unwrap(), "/bin/zsh");
    }

    #[test]
    fn format_command_substitutes_host() {
        let config = ConnectProgramConfig::new("connect to %H".into(), None).unwrap();
        let result = config.format_command("example.com", 873).unwrap();
        assert_eq!(result, "connect to 'example.com'");
    }

    #[test]
    fn format_command_substitutes_port() {
        let config = ConnectProgramConfig::new("port %P".into(), None).unwrap();
        let result = config.format_command("host", 8080).unwrap();
        assert_eq!(result, "port 8080");
    }

    #[test]
    fn format_command_substitutes_both() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("server.local", 22).unwrap();
        assert_eq!(result, "nc 'server.local' 22");
    }

    #[test]
    fn format_command_escapes_percent() {
        let config = ConnectProgramConfig::new("echo 100%%".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "echo 100%");
    }

    #[test]
    fn format_command_preserves_unknown_specifiers() {
        let config = ConnectProgramConfig::new("echo %X".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "echo %X");
    }

    #[test]
    fn format_command_trailing_percent() {
        let config = ConnectProgramConfig::new("test%".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "test%");
    }

    #[test]
    fn format_command_multiple_substitutions() {
        let config = ConnectProgramConfig::new("%H:%P and %H again".into(), None).unwrap();
        let result = config.format_command("myhost", 9999).unwrap();
        assert_eq!(result, "'myhost':9999 and 'myhost' again");
    }

    #[test]
    fn format_command_no_substitutions() {
        let config = ConnectProgramConfig::new("static command".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "static command");
    }

    #[test]
    fn format_command_adjacent_specifiers() {
        let config = ConnectProgramConfig::new("%H%P".into(), None).unwrap();
        let result = config.format_command("test", 123).unwrap();
        assert_eq!(result, "'test'123");
    }

    #[test]
    fn format_command_complex_template() {
        let config =
            ConnectProgramConfig::new("ssh -p %P %H -o 'Port=%P' %% done".into(), None).unwrap();
        let result = config.format_command("server", 2222).unwrap();
        assert_eq!(result, "ssh -p 2222 'server' -o 'Port=2222' % done");
    }

    #[test]
    fn format_command_ipv6_host() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("::1", 873).unwrap();
        assert_eq!(result, "nc '::1' 873");
    }

    /// An empty host is refused, not substituted.
    ///
    /// upstream: socket.c:208 - `if (!*host || ...) return 1`. It survives a
    /// direct exec as an empty argument but vanishes when a nested shell
    /// re-splits the command, shifting every later argument left.
    #[test]
    fn format_command_empty_host_is_refused() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        assert_eq!(
            config.format_command("", 873),
            Err(UNSAFE_CONNECT_HOST.to_owned())
        );
    }

    #[test]
    fn format_command_port_zero() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("host", 0).unwrap();
        assert_eq!(result, "nc 'host' 0");
    }

    #[test]
    fn format_command_port_max() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("host", 65535).unwrap();
        assert_eq!(result, "nc 'host' 65535");
    }

    /// `@` is outside upstream's allowed byte set, so a `user@host` form is
    /// refused rather than quoted.
    ///
    /// upstream: socket.c:211 - the set is alphanumerics plus `._:-%+~`.
    #[test]
    fn format_command_refuses_a_byte_outside_the_allowed_set() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        assert_eq!(
            config.format_command("user@host.example.com", 22),
            Err(UNSAFE_CONNECT_HOST.to_owned())
        );
    }

    /// Every host the upstream cell expects to be refused.
    ///
    /// upstream: `testsuite/connect-prog-host-quoting_test.py`. The first four
    /// are refused by the leading-byte rule (they change an argument's meaning,
    /// which quoting cannot undo); the injection string is refused because `;`,
    /// space, `$`, `{` and `#` are all outside the allowed set.
    #[test]
    fn format_command_refuses_every_unsafe_host_from_the_upstream_cell() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        for host in [
            "localhost;touch${IFS}pwned;#",
            "-c",
            "+x",
            "~root",
            "%self",
            "",
        ] {
            assert_eq!(
                config.format_command(host, 873),
                Err(UNSAFE_CONNECT_HOST.to_owned()),
                "host {host:?} must be refused"
            );
        }
    }

    /// Every host the upstream cell expects to survive, quoted.
    ///
    /// upstream: `testsuite/connect-prog-host-quoting_test.py`. `+`, `~` and
    /// `%` are legal mid-word - `%` in particular is required for an IPv6 zone
    /// id, which the cell passes as `[fe80::1%eth0]` and rsync unwraps to
    /// `fe80::1%eth0` before this point.
    #[test]
    fn format_command_accepts_every_safe_host_from_the_upstream_cell() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        for host in [
            "example.com",
            "example.com.",
            "host_name",
            "host+alias",
            "host~alias",
            "fe80::1%eth0",
        ] {
            assert_eq!(
                config.format_command(host, 873),
                Ok(format!("nc '{host}' 873").into()),
                "host {host:?} must pass through quoted"
            );
        }
    }

    /// A template with no specifier never inspects the host at all.
    ///
    /// upstream: socket.c:488 - the refusal, the quoting and the substitution
    /// all sit inside `if (prog && strchr(prog, '%'))`. Without this gate a
    /// fixed command such as `nc localhost 873` would start failing for hosts
    /// it never interpolates.
    #[test]
    fn format_command_skips_host_validation_without_a_specifier() {
        let config = ConnectProgramConfig::new("nc localhost 873".into(), None).unwrap();
        let result = config.format_command("localhost;touch pwned", 873).unwrap();
        assert_eq!(result, "nc localhost 873");
    }

    /// The `'\''` escape is upstream's backstop, unreachable through
    /// `format_command` because `'` is outside the allowed byte set. Pinned
    /// directly so the two halves cannot drift apart.
    ///
    /// upstream: socket.c:152 `shell_quote_connect_host()`.
    #[test]
    fn shell_quote_renders_an_embedded_quote_as_the_escape_sequence() {
        assert_eq!(shell_quote_connect_host("a'b"), r"'a'\''b'");
        assert!(shell_unsafe_connect_host("a'b"));
    }

    /// Verifies that a child process spawned via `connect_via_program` receives
    /// a socket (not a pipe) as its stdin, so daemon `is_a_socket()` detection
    /// succeeds.
    ///
    /// The test spawns a shell one-liner that writes `getsockopt(SO_TYPE)` probe
    /// output back through the socketpair. On Unix, stdin must be a socket. On
    /// non-Unix, this test is skipped since inetd detection does not apply.
    #[cfg(unix)]
    #[test]
    fn connect_program_child_stdin_is_socket() {
        use std::io::BufRead;

        // Python one-liner that probes stdin with getsockopt(SO_TYPE) and prints
        // "SOCKET" or "NOT_SOCKET" to stdout (which goes back through the
        // socketpair to the parent).
        let probe_script = concat!(
            "import socket, sys; ",
            "s = socket.fromfd(sys.stdin.fileno(), socket.AF_UNIX, socket.SOCK_STREAM); ",
            "print('SOCKET' if s.getsockopt(socket.SOL_SOCKET, socket.SO_TYPE) else 'NOT_SOCKET'); ",
            "sys.stdout.flush()"
        );

        let config =
            ConnectProgramConfig::new(format!("python3 -c \"{probe_script}\"").into(), None)
                .unwrap();

        let addr = DaemonAddress::new("localhost".to_owned(), 873);
        let result = connect_via_program(&addr, &config);

        // If python3 is not available, skip rather than fail.
        let Ok(mut stream) = result else {
            eprintln!("skipping: python3 not available for socketpair probe test");
            return;
        };

        let mut reader = io::BufReader::new(&mut stream);
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => {
                eprintln!("skipping: could not read from connect program");
                return;
            }
            Ok(_) => {}
        }

        assert_eq!(
            line.trim(),
            "SOCKET",
            "child stdin should be a socket, not a pipe"
        );
    }

    /// Verifies bidirectional data flow through the socketpair transport.
    ///
    /// Spawns a `cat` process that echoes stdin to stdout. Since both are
    /// connected to the same socketpair end, writing to the parent socket and
    /// reading back should round-trip the data.
    #[cfg(unix)]
    #[test]
    fn connect_program_socketpair_bidirectional() {
        let config = ConnectProgramConfig::new("cat".into(), None).unwrap();
        let addr = DaemonAddress::new("localhost".to_owned(), 873);
        let mut stream = connect_via_program(&addr, &config).unwrap();

        let message = b"hello socketpair\n";
        stream.write_all(message).unwrap();
        stream.flush().unwrap();

        let mut buf = vec![0u8; message.len()];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, message);
    }

    /// Verifies that `into_parts()` produces working read/write halves
    /// from a socketpair-backed stream.
    #[cfg(unix)]
    #[test]
    fn connect_program_into_parts_round_trip() {
        use std::process::Command;

        let (parent_sock, child_sock) = socketpair_tcp().unwrap();
        let child_sock_dup = child_sock.try_clone().unwrap();

        let child_stdin = Stdio::from(std::os::fd::OwnedFd::from(child_sock));
        let child_stdout = Stdio::from(std::os::fd::OwnedFd::from(child_sock_dup));

        let child = Command::new("cat")
            .stdin(child_stdin)
            .stdout(child_stdout)
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();

        let stream = ConnectProgramStream::from_socketpair(child, parent_sock);
        let mut parts = stream.into_parts().unwrap();

        let message = b"split test\n";
        parts.writer.write_all(message).unwrap();
        parts.writer.flush().unwrap();

        let mut buf = vec![0u8; message.len()];
        parts.reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, message);

        let _ = parts.child.kill();
        let _ = parts.child.wait();
    }

    /// A connect program that keeps working after the last transfer byte must
    /// be allowed to finish. A daemon reached through `RSYNC_CONNECT_PROG` runs
    /// its `post-xfer exec` hook there, so signalling the child - as oc did -
    /// destroys it.
    ///
    /// The fixture writes its marker only *after* stdin reaches EOF and a short
    /// delay, so a child that is killed on drop deterministically leaves no
    /// marker.
    ///
    /// upstream: socket.c:1046 `sock_exec()` never signals the child.
    #[cfg(unix)]
    #[test]
    fn dropping_the_stream_lets_the_connect_program_finish_after_eof() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("post.out");

        let config = ConnectProgramConfig::new(
            format!(
                "cat >/dev/null; sleep 0.2; printf done > {}",
                marker.display()
            )
            .into(),
            None,
        )
        .unwrap();
        let addr = DaemonAddress::new("localhost".to_owned(), 873);
        let stream = connect_via_program(&addr, &config).unwrap();

        drop(stream);

        assert_eq!(
            std::fs::read_to_string(&marker).ok().as_deref(),
            Some("done"),
            "the connect program must reach its post-transfer work and be waited for"
        );
    }

    /// Same contract on the split path taken by every daemon transfer: the
    /// halves close first, then `DaemonStreamGuard::finish` waits. Without the
    /// wait the marker is not yet there; with a kill it never arrives at all.
    #[cfg(unix)]
    #[test]
    fn finishing_the_split_guard_waits_for_the_connect_program() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("post.out");

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "cat >/dev/null; sleep 0.2; printf done > {}",
                marker.display()
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let (reader, writer, guard) =
            crate::client::module_list::DaemonStream::from_child_process(child, stdin, stdout)
                .split()
                .unwrap();
        drop(writer);
        drop(reader);
        guard.finish();

        assert_eq!(
            std::fs::read_to_string(&marker).ok().as_deref(),
            Some("done"),
            "finish() must wait for the child that the closed halves released"
        );
    }
}
