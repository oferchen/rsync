//! Which upstream process reports a failed remote transfer.
//!
//! A pushing upstream client is one process, the sender. A pulling client
//! receives the file list as one process (`who_am_i()` = `"Receiver"`), then
//! forks in `do_recv()` (main.c:1077): the child becomes the receiver and the
//! parent the generator, and each reports its own failures (rsync.c:987-995).
//! oc runs both halves in one context, so it names the process upstream would
//! from the [`ForkPoint`] the receiver marks.
//!
//! [`local_failure_error`] is the one class mapping both the remote-shell and
//! the daemon transports use for a failure of the client's own half.

use std::io;

use engine::local_copy::upstream_io_error;

use crate::client::error::{
    ClientError, cause_line_error, connection_unexpectedly_closed_error, local_abort_text,
    program_line_error, remote_exit_error,
};
use crate::exit_code::ExitCode;
use crate::message::Role;
use crate::server::{DestRootMkdirError, ForkPoint};

/// The process that reports a failure, by the client's role and, on a pull,
/// the side of upstream's fork the failure struck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReportingSide {
    /// A pushing client: one process, the sender.
    Sender,
    /// A pulling client before the fork: the lone `Receiver`.
    PreFork,
    /// A pulling client after the fork, with the raw bytes the generator read.
    PostFork {
        /// The generator's `stats.total_read` when the stream ended.
        generator_read: u64,
    },
}

impl ReportingSide {
    /// The side a client of `local_role` is on once its transfer stopped.
    pub(crate) fn of(local_role: Role, fork_point: &ForkPoint) -> Self {
        match (local_role, fork_point.total_read_at_fork()) {
            (Role::Sender, _) => Self::Sender,
            (_, None) => Self::PreFork,
            (_, Some(generator_read)) => Self::PostFork { generator_read },
        }
    }

    /// The process whose exit trailer closes the run: after the fork the
    /// generator, the parent, is the last to exit (main.c:1142).
    pub(crate) const fn exit_role(self) -> Role {
        match self {
            Self::Sender => Role::Sender,
            Self::PreFork => Role::PreForkReceiver,
            Self::PostFork { .. } => Role::Generator,
        }
    }

    /// The process that hits a failure of the client's own receive: the
    /// receiver child once forked.
    pub(crate) const fn local_role(self) -> Role {
        match self {
            Self::Sender => Role::Sender,
            Self::PreFork => Role::PreForkReceiver,
            Self::PostFork { .. } => Role::Receiver,
        }
    }

    /// Completes the report of a fatal failure in the client's own receive.
    ///
    /// upstream: cleanup.c:240-256 - a receiver child exiting with any code but
    /// `RERR_SOCKETIO`, `RERR_STREAMIO`, `RERR_SIGNAL1` or `RERR_TIMEOUT` sends
    /// `MSG_ERROR_EXIT`, and the generator exits silently with that code. For
    /// those four it sends nothing, so the generator meets end-of-file on the
    /// error pipe, whines with its own read count and exits `RERR_STREAMIO`
    /// (io.c:299-303), after the receiver's report.
    ///
    /// `generator_exit` is the generator's code, `RERR_STREAMIO` or the
    /// remote-shell child's worse status (cleanup.c:146-153).
    pub(crate) fn finish(self, report: ClientError, generator_exit: ExitCode) -> ClientError {
        match self {
            Self::PostFork { generator_read }
                if matches!(
                    report.code(),
                    ExitCode::SocketIo | ExitCode::StreamIo | ExitCode::Signal1 | ExitCode::Timeout
                ) =>
            {
                connection_unexpectedly_closed_error(
                    generator_read,
                    generator_exit,
                    Role::Generator,
                )
                .after(report)
            }
            _ => report,
        }
    }
}

/// Maps a failure of the client's own half of a transfer to upstream's report.
///
/// `child_exit` is the remote-shell child's status, `None` on a daemon socket;
/// exit_cleanup() keeps the worse of it and the local code (cleanup.c:146-153).
/// `io_timeout` is the `--timeout` in force on the connection, in seconds.
pub(crate) fn local_failure_error(
    error: &io::Error,
    child_exit: Option<ExitCode>,
    side: ReportingSide,
    bytes_received: u64,
    io_timeout: Option<u64>,
) -> ClientError {
    let role = side.local_role();
    let who = role.as_str();
    let worse = |code: ExitCode| {
        child_exit
            .filter(|child| child.as_i32() > code.as_i32())
            .unwrap_or(code)
    };
    // upstream: main.c:817-821 get_local_name() - rsyserr() then
    // exit_cleanup(RERR_FILEIO), before the remote sender has finished.
    if let Some(failed) = DestRootMkdirError::of(error) {
        let line = format!(
            "[{who}] mkdir {} failed: {}",
            failed.fname(),
            upstream_io_error(failed.source_error())
        );
        return program_line_error(line, ExitCode::FileIo, role);
    }
    // upstream: io.c:282-304 whine_about_eof() - the peer closed the stream.
    // The forked receiver cannot reap the parent's child, so only a lone
    // receiver takes its status.
    if error.kind() == io::ErrorKind::UnexpectedEof {
        let own = match side {
            ReportingSide::PostFork { .. } => ExitCode::StreamIo,
            _ => worse(ExitCode::StreamIo),
        };
        let report = connection_unexpectedly_closed_error(bytes_received, own, role);
        return side.finish(report, worse(ExitCode::StreamIo));
    }
    // upstream: io.c:259-266 check_timeout() - no input within --timeout.
    if let Some(seconds) = io_timeout
        && matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )
    {
        return timeout_error(side, seconds);
    }
    // upstream: io.c:930-946 perform_io() - a failed read of the peer's
    // stream is rsyserr(FERROR_SOCKET, errno, "read error") and
    // exit_cleanup(RERR_SOCKETIO).
    if error.raw_os_error().is_some()
        && matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        )
    {
        let line = format!("[{who}] read error: {}", upstream_io_error(error));
        let report = program_line_error(line, ExitCode::SocketIo, role);
        return side.finish(report, ExitCode::StreamIo);
    }
    let transfer_exit = ExitCode::from_io_error(error);
    // upstream: cleanup.c:146-153 - _exit_cleanup() polls the child with
    // WNOHANG. When the client itself aborts with RERR_PROTOCOL (e.g.
    // exclude.c:1924-1926 refusing a filter rule a pre-29 peer cannot read),
    // the remote side is still blocked on its input, so its status is not yet
    // available and the local code stands. oc closes the pipes and waits,
    // which makes the child exit with its own EOF code (12) and would
    // otherwise mask the refusal. A receiver that cannot enter its destination
    // (main.c:778-781, RERR_FILESELECT) aborts the same way, before the remote
    // sender has finished. A rule refused out of a per-directory merge file
    // during the sender's walk is the same: exclude.c:133-137 filter_rule_err()
    // exits RERR_SYNTAX while the peer still waits for the file list.
    let local_setup_abort = matches!(
        transfer_exit,
        ExitCode::Protocol | ExitCode::FileSelect | ExitCode::Syntax
    );
    let report = match child_exit {
        Some(child) if !local_setup_abort && child.as_i32() > transfer_exit.as_i32() => {
            remote_exit_error(child, side.exit_role())
        }
        _ => crate::client::error::invalid_argument_error_typed_with_role(
            &local_abort_text(error),
            transfer_exit,
            role,
        ),
    };
    side.finish(report, ExitCode::StreamIo)
}

/// Reports a peer silent for longer than `--timeout`.
///
/// upstream: io.c:256-266 check_timeout() returns early for `am_receiver`, so
/// once forked it is the generator that times out. Its exit_cleanup() then
/// sends the receiver SIGUSR1 (cleanup.c:202-203), whose handler exits
/// `RERR_SIGNAL1` with its own trailer (main.c:1684-1688); the generator's
/// `RERR_TIMEOUT` is the run's exit code.
fn timeout_error(side: ReportingSide, seconds: u64) -> ClientError {
    let cause = |role: Role| {
        let line = format!("[{role}] io timeout after {seconds} seconds -- exiting");
        cause_line_error(line, ExitCode::Timeout, role)
    };
    match side {
        ReportingSide::PostFork { .. } => remote_exit_error(ExitCode::Signal1, Role::Receiver)
            .with_exit_code(ExitCode::Timeout)
            .after(cause(Role::Generator)),
        _ => cause(side.local_role()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::error::remote_exit_error;

    fn forked_at(total_read: u64) -> ForkPoint {
        let point = ForkPoint::default();
        point.mark(total_read);
        point
    }

    #[test]
    fn a_push_is_always_the_sender() {
        assert_eq!(
            ReportingSide::of(Role::Sender, &forked_at(10)),
            ReportingSide::Sender
        );
    }

    #[test]
    fn a_pull_is_the_lone_receiver_until_the_fork() {
        let side = ReportingSide::of(Role::PreForkReceiver, &ForkPoint::default());
        assert_eq!(side, ReportingSide::PreFork);
        assert_eq!(side.exit_role().as_str(), "Receiver");
        assert_eq!(side.local_role().as_str(), "Receiver");
    }

    #[test]
    fn after_the_fork_the_receiver_fails_and_the_generator_exits() {
        let side = ReportingSide::of(Role::PreForkReceiver, &forked_at(58));
        assert_eq!(side, ReportingSide::PostFork { generator_read: 58 });
        assert_eq!(side.local_role(), Role::Receiver);
        assert_eq!(side.exit_role(), Role::Generator);
    }

    /// The four codes cleanup.c:241-242 keeps off the error pipe leave the
    /// generator to whine and exit 12 after the receiver's own report.
    #[test]
    fn a_stream_class_failure_after_the_fork_ends_on_the_generator_whine() {
        let side = ReportingSide::PostFork { generator_read: 79 };
        for code in [
            ExitCode::SocketIo,
            ExitCode::StreamIo,
            ExitCode::Signal1,
            ExitCode::Timeout,
        ] {
            let report = side.finish(remote_exit_error(code, Role::Receiver), ExitCode::StreamIo);
            assert_eq!(report.code(), ExitCode::StreamIo, "{code:?}");
            assert_eq!(
                report.program_line(),
                Some("connection unexpectedly closed (79 bytes received so far) [generator]")
            );
            assert!(report.to_string().contains("[generator="), "{report}");
            let earlier = report.preceding().expect("receiver report first");
            assert_eq!(earlier.code(), code);
            assert!(earlier.to_string().contains("[receiver="), "{earlier}");
        }
    }

    /// Any other code reaches the generator as MSG_ERROR_EXIT, which exits
    /// with it silently.
    #[test]
    fn other_failures_after_the_fork_end_on_the_receiver_report() {
        let side = ReportingSide::PostFork { generator_read: 79 };
        let report = side.finish(
            remote_exit_error(ExitCode::FileIo, Role::Receiver),
            ExitCode::StreamIo,
        );
        assert_eq!(report.code(), ExitCode::FileIo);
        assert!(report.preceding().is_none());
    }

    /// After the fork a closed stream reaches both processes: the receiver
    /// whines with its own count and 12, the generator with the count it
    /// inherited, and the run takes the worse of 12 and the child's status
    /// (cleanup.c:146-153), which the receiver child cannot reap.
    #[test]
    fn an_eof_after_the_fork_reports_receiver_then_generator() {
        let eof = io::Error::from(io::ErrorKind::UnexpectedEof);
        let side = ReportingSide::PostFork { generator_read: 58 };
        let report = local_failure_error(&eof, Some(ExitCode::Signal), side, 1954, None);
        assert_eq!(report.code(), ExitCode::Signal);
        assert_eq!(
            report.program_line(),
            Some("connection unexpectedly closed (58 bytes received so far) [generator]")
        );
        let receiver = report.preceding().expect("the receiver reports first");
        assert_eq!(receiver.code(), ExitCode::StreamIo);
        assert_eq!(
            receiver.program_line(),
            Some("connection unexpectedly closed (1954 bytes received so far) [receiver]")
        );
    }

    /// Before the fork the lone `Receiver` reads the stream; a reset there is
    /// its `read error` and RERR_SOCKETIO, with nobody after it.
    #[test]
    fn a_reset_before_the_fork_is_the_receivers_read_error() {
        let reset = io::Error::from_raw_os_error(if cfg!(windows) {
            10054
        } else if cfg!(target_os = "linux") {
            104
        } else {
            54
        });
        let report = local_failure_error(&reset, None, ReportingSide::PreFork, 10, None);
        assert_eq!(report.code(), ExitCode::SocketIo);
        let line = report.program_line().expect("read error line");
        assert!(line.starts_with("[Receiver] read error: "), "{line}");
        assert!(report.preceding().is_none());
    }

    /// A file-I/O failure after the fork reaches the generator as
    /// MSG_ERROR_EXIT, so the receiver's report is the last word and its code
    /// the run's (cleanup.c:241-251).
    #[test]
    fn a_file_io_failure_after_the_fork_ends_on_the_receiver() {
        let full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
        let side = ReportingSide::PostFork { generator_read: 58 };
        let report = local_failure_error(&full, None, side, 1954, None);
        assert_eq!(report.code(), ExitCode::FileIo);
        assert!(report.to_string().contains("[receiver="), "{report}");
        assert!(report.preceding().is_none());
    }

    #[test]
    fn before_the_fork_there_is_no_generator_to_report() {
        let report = ReportingSide::PreFork.finish(
            remote_exit_error(ExitCode::StreamIo, Role::PreForkReceiver),
            ExitCode::StreamIo,
        );
        assert!(report.preceding().is_none());
        assert!(report.program_line().is_none());
    }
}
