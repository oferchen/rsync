//! The exit status of a failed protocol-version exchange with a server.
//!
//! upstream: compat.c:606-607 setup_protocol() - the client writes its version
//! and reads the peer's before buffered I/O starts, so both go through io.c
//! safe_write()/safe_read(). Each way that exchange fails exits `RERR_STREAMIO`,
//! or `RERR_PROTOCOL` for an out-of-range version (compat.c:621-625).
//! `RERR_STARTCLIENT` is only main.c:1981, where start_client() fails before
//! any I/O, so a remote shell that dies first never reaches it.

use std::io;

use engine::local_copy::upstream_io_error;

use super::super::error::{
    ClientError, connection_unexpectedly_closed_error, invalid_argument_error_typed_with_role,
    program_line_error, remote_exit_error,
};
use crate::exit_code::ExitCode;
use crate::message::Role;
use crate::server::{VersionExchangeError, VersionExchangeStep};

/// Builds the client error for a failed version exchange.
///
/// `child_exit` is the remote-shell child's mapped exit status when it was
/// reaped. exit_cleanup() keeps the worse of it and the local code
/// (cleanup.c:150-152); the diagnostic line stays the local one.
pub(crate) fn version_exchange_failure(
    error: &io::Error,
    child_exit: Option<ExitCode>,
    role: Role,
) -> ClientError {
    let base = if ExitCode::from_io_error(error) == ExitCode::Protocol {
        ExitCode::Protocol
    } else {
        ExitCode::StreamIo
    };
    let exit = child_exit
        .filter(|child| child.as_i32() > base.as_i32())
        .unwrap_or(base);
    if error.kind() == io::ErrorKind::UnexpectedEof {
        // upstream: io.c:299-303 whine_about_eof(); nothing is counted before
        // buffered input starts (main.c:1378-1381).
        return connection_unexpectedly_closed_error(0, exit, role);
    }
    if let Some(failed) = VersionExchangeError::of(error) {
        return program_line_error(safe_io_line(failed, role), exit, role);
    }
    if exit != base {
        // upstream: log.c:937 log_exit() names the winning child code.
        return remote_exit_error(exit, role);
    }
    invalid_argument_error_typed_with_role(&format!("handshake failed: {error}"), base, role)
}

/// The rsyserr() line safe_write()/safe_read() print for a failed step.
///
/// upstream: io.c:401-405 safe_write() - `"safe_write failed to write %d bytes
/// to %s"`, where what_fd_is() names the client's `sock_f_out` "socket";
/// io.c:356-358 safe_read() - `"safe_read failed to read %d bytes"`. rsyserr()
/// (log.c) prefixes `[who_am_i()]` and appends `": %s (%d)"`.
fn safe_io_line(failed: &VersionExchangeError, role: Role) -> String {
    let len = VersionExchangeError::LEN;
    let cause = upstream_io_error(failed.source_error());
    let who = role.as_str();
    match failed.step() {
        VersionExchangeStep::Write => {
            format!("[{who}] safe_write failed to write {len} bytes to socket: {cause}")
        }
        VersionExchangeStep::Read => {
            format!("[{who}] safe_read failed to read {len} bytes: {cause}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;
    use crate::server::perform_handshake_with_max;
    use protocol::ProtocolVersion;

    /// A writer whose peer already closed its read end.
    struct ClosedPipe(fn() -> io::Error);

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err((self.0)())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn broken_pipe() -> io::Error {
        io::Error::from(io::ErrorKind::BrokenPipe)
    }

    fn exchange(reader: &[u8], writer: &mut dyn Write) -> io::Error {
        perform_handshake_with_max(
            &mut Cursor::new(reader.to_vec()),
            writer,
            ProtocolVersion::NEWEST,
        )
        .expect_err("the exchange fails")
    }

    /// The write-first ordering of a remote shell that died before the
    /// handshake: 3.5.1 exits 12 with its safe_write() line, never the
    /// RERR_STARTCLIENT (5) oc used to report.
    /// EPIPE is 32 on Linux and the BSDs, as the oracle line shows.
    #[cfg(unix)]
    #[test]
    fn version_write_epipe_exits_stream_io_with_the_safe_write_line() {
        let error = exchange(&[], &mut ClosedPipe(|| io::Error::from_raw_os_error(32)));
        let failure = version_exchange_failure(&error, None, Role::Sender);
        assert_eq!(failure.code(), ExitCode::StreamIo);
        assert_eq!(
            failure.program_line(),
            Some("[sender] safe_write failed to write 4 bytes to socket: Broken pipe (32)")
        );
    }

    /// The read-first ordering: EOF before the peer's version is the
    /// whine_about_eof() line, with the same exit code.
    #[test]
    fn version_read_eof_exits_stream_io_with_the_eof_whine() {
        let error = exchange(&[], &mut Vec::new());
        let failure = version_exchange_failure(&error, None, Role::Receiver);
        assert_eq!(failure.code(), ExitCode::StreamIo);
        let line = failure.program_line().expect("whine line");
        assert!(
            line.starts_with("connection unexpectedly closed (0 bytes received so far)"),
            "{line}"
        );
    }

    /// Any write failure, errno or not, is RERR_STREAMIO, not RERR_SOCKETIO:
    /// safe_write() exits 12 whatever the errno (io.c:401-405).
    #[test]
    fn any_version_write_failure_exits_stream_io() {
        let error = exchange(&[], &mut ClosedPipe(broken_pipe));
        assert_eq!(
            version_exchange_failure(&error, None, Role::Sender).code(),
            ExitCode::StreamIo
        );
    }

    /// A read that fails without EOF prints the safe_read() line and is
    /// still RERR_STREAMIO (io.c:356-358).
    #[test]
    fn version_read_error_exits_stream_io_with_the_safe_read_line() {
        struct ResetPeer;
        impl io::Read for ResetPeer {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::ConnectionReset))
            }
        }
        let error =
            perform_handshake_with_max(&mut ResetPeer, &mut Vec::new(), ProtocolVersion::NEWEST)
                .expect_err("the read fails");
        let failure = version_exchange_failure(&error, None, Role::Receiver);
        assert_eq!(failure.code(), ExitCode::StreamIo);
        let line = failure.program_line().expect("safe_read line");
        assert!(
            line.starts_with("[receiver] safe_read failed to read 4 bytes: "),
            "{line}"
        );
    }

    /// A worse raw child status names the trailer; the local line stays.
    #[test]
    fn worse_child_exit_wins_the_code_but_keeps_the_safe_write_line() {
        let error = exchange(&[], &mut ClosedPipe(broken_pipe));
        let failure =
            version_exchange_failure(&error, Some(ExitCode::CommandNotFound), Role::Sender);
        assert_eq!(failure.code(), ExitCode::CommandNotFound);
        assert!(
            failure
                .program_line()
                .is_some_and(|line| line.contains("safe_write failed"))
        );
    }

    /// An out-of-range peer version stays RERR_PROTOCOL (compat.c:621-625).
    #[test]
    fn bad_peer_version_exits_protocol() {
        let error = exchange(&[0, 0, 0, 0], &mut Vec::new());
        let failure = version_exchange_failure(&error, None, Role::Sender);
        assert_eq!(failure.code(), ExitCode::Protocol);
    }
}
