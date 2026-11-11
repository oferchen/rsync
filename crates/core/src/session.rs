//! Session-level entry point for the native server over standard I/O.
//!
//! `core` owns the transfer pipeline driver. The public entry point
//! (`run_server_stdio`) forwards directly to the threaded
//! `transfer::run_server_stdio`.

mod confinement;

pub use confinement::confine_receiver;

use std::io::{Read, Write};

use crate::branding::Brand;
use crate::exit_code::ExitCode;
use crate::message::Role;
use crate::server::{ForkPoint, ServerConfig, ServerResult, ServerRole, ServerStats};
use transfer::TransferProgressCallback;

/// Executes the native server over standard I/O.
///
/// This is the session-level facade the `--server` entry point calls.
///
/// # Errors
///
/// Propagates every error from the underlying server body unchanged.
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    progress: Option<&mut dyn TransferProgressCallback>,
) -> ServerResult {
    transfer::run_server_stdio(config, stdin, stdout, progress)
}

/// Executes the native server over the process's own standard input and
/// output descriptors, which the transfer polls together.
///
/// `stdin` and `stdout` are handles on descriptors 0 and 1.
///
/// # Errors
///
/// Propagates every error from the underlying server body unchanged.
#[cfg(unix)]
pub fn run_server_fds(
    config: ServerConfig,
    stdin: std::fs::File,
    stdout: std::fs::File,
    progress: Option<&mut dyn TransferProgressCallback>,
    fork_point: Option<std::sync::Arc<crate::server::ForkPoint>>,
) -> ServerResult {
    transfer::run_server_fds(config, stdin, stdout, progress, fork_point)
}

/// Turns a finished server body into the status its process exits with,
/// printing the report a failing server writes on its own stderr.
///
/// Shared by the `--server` process and a local copy's receiver thread, which
/// is upstream's forked `--server` child (pipe.c:109-160 local_child()).
///
/// upstream: cleanup.c:210-218 - a clean finish exits with the code the
/// accumulated io_error bits select (23/24/25).
#[must_use]
pub fn server_exit_status(
    outcome: ServerResult,
    role: ServerRole,
    fork_point: &ForkPoint,
    brand: Brand,
    stderr: &mut dyn Write,
) -> i32 {
    let e = match outcome {
        Ok(stats) => {
            let (io_error, got_xfer_error) = match stats {
                ServerStats::Receiver(s) => (s.io_error, s.got_xfer_error),
                ServerStats::Generator(s) => (s.io_error, s.got_xfer_error),
            };
            return crate::exit_code::io_error_exit_code(io_error, got_xfer_error).unwrap_or(0);
        }
        Err(e) => e,
    };
    // upstream: io.c:1930 - an exit the peer asked for with MSG_ERROR_EXIT
    // re-enters _exit_cleanup() with a negative line, so log_exit() prints
    // nothing (cleanup.c:221-226) and the process leaves with the peer's code.
    if let Some(code) = crate::server::remote_exit_code(&e) {
        return code;
    }
    let signalled = crate::signal::shutdown_reason().is_some();
    // upstream: cleanup.c:241-256 - a forked receiver dying with one of the
    // unannounced codes flushes none of its queued messages, so its own report
    // never leaves it; the generator meets end-of-file on the error pipe and
    // reports that instead (io.c:299-303). A signal exits with RERR_SIGNAL
    // first (io.c:600-605), which is announced.
    if !signalled
        && role == ServerRole::Receiver
        && let Some(generator_read) = fork_point.total_read_at_fork()
        && crate::exit_code::receiver_exit_is_unannounced(
            crate::exit_code::cause_line_exit_code(&e)
                .unwrap_or_else(|| ExitCode::from_io_error(&e)),
        )
    {
        for line in crate::exit_code::generator_eof_report_lines(generator_read, brand) {
            let _ = stderr.write_all(&line);
        }
        return ExitCode::StreamIo.as_i32();
    }
    let who = if role == ServerRole::Generator {
        Role::Sender
    } else {
        Role::Generator
    };
    report_server_failure(stderr, brand, who, &e, signalled)
}

/// Reports the failure that ended a server transfer and returns the exit code.
///
/// A pending signal wins over whatever error the transfer was unwinding with:
/// upstream's handle_kill_signal() calls exit_cleanup(RERR_SIGNAL) itself, so
/// log_exit() prints the bare trailer and no cause line, even when the code
/// path it interrupted had one to print.
///
/// upstream: io.c:600-605 handle_kill_signal(), log.c:937-963 log_exit()
fn report_server_failure(
    stderr: &mut dyn Write,
    brand: Brand,
    who: Role,
    error: &std::io::Error,
    signalled: bool,
) -> i32 {
    if signalled {
        let code = ExitCode::Signal;
        write_trailer(stderr, brand, who, code, code.description());
        return code.as_i32();
    }
    if let Some(code) = crate::exit_code::cause_line_exit_code(error) {
        for line in crate::exit_code::fatal_report_lines(error, code, who, brand) {
            let _ = stderr.write_all(&line);
        }
        return code.as_i32();
    }
    let code = ExitCode::from_io_error(error);
    // upstream: receiver.c:658-660 and the other RERR_FILEIO sites of the
    // receive path print their own line first; log_exit() then prints only
    // the code's description (cleanup.c:248).
    let text = if matches!(
        crate::server::temp_guard::CommitDisposition::of(error),
        crate::server::temp_guard::CommitDisposition::Abort(_)
    ) {
        code.description().to_owned()
    } else {
        format!("server error: {error}")
    };
    write_trailer(stderr, brand, Role::Server, code, &text);
    code.as_i32()
}

/// Prints log_exit()'s `rsync error:` trailer carrying `code`, naming the
/// process that hit it.
///
/// upstream: log.c:937-963 log_exit()
fn write_trailer(stderr: &mut dyn Write, brand: Brand, role: Role, code: ExitCode, text: &str) {
    let trailer = crate::rsync_error!(code.as_i32(), "{}", text)
        .with_role(role)
        .with_brand(brand);
    if let Ok(line) = trailer.to_line_bytes() {
        let _ = stderr.write_all(&line);
    }
}

#[cfg(test)]
mod failure_report_tests {
    use super::*;

    /// A signal that arrives while a failure with its own cause line is
    /// unwinding reports the signal alone: handle_kill_signal() exits with
    /// RERR_SIGNAL before the interrupted site prints anything.
    ///
    /// upstream: io.c:600-605, log.c:937-963
    #[test]
    fn a_signal_reports_only_its_trailer_over_a_pending_cause() {
        let pending = ::protocol::malloc_failure("[receiver] exceeded --max-alloc=1048576 setting");

        let mut unsignalled = Vec::new();
        let code = report_server_failure(
            &mut unsignalled,
            Brand::Upstream,
            Role::Generator,
            &pending,
            false,
        );
        let unsignalled = String::from_utf8(unsignalled).expect("utf-8");
        assert_eq!(code, 22);
        assert!(
            unsignalled.starts_with("[receiver] exceeded --max-alloc"),
            "the fixture's failure prints a cause line of its own:\n{unsignalled}"
        );

        let mut signalled = Vec::new();
        let code = report_server_failure(
            &mut signalled,
            Brand::Upstream,
            Role::Generator,
            &pending,
            true,
        );
        let signalled = String::from_utf8(signalled).expect("utf-8");
        assert_eq!(code, 20);
        assert_eq!(signalled.lines().count(), 1, "{signalled}");
        assert!(
            signalled.starts_with("rsync error: received SIGINT, SIGTERM, or SIGHUP (code 20) at "),
            "{signalled}"
        );
        assert!(signalled.contains("[generator="), "{signalled}");
    }
}

