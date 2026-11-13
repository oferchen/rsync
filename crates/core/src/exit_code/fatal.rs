//! What a server reports for a failure that ends its transfer: the cause
//! upstream prints with `rprintf(FERROR, ...)`, then log_exit()'s trailer.

use std::io;

use engine::local_copy::upstream_io_error;
use transfer::DestRootMkdirError;

use crate::branding::Brand;

use super::ExitCode;
use crate::message::Role;

/// The exit code of a failure upstream reports as `rprintf(FERROR, cause)`
/// followed by `exit_cleanup(code)`, or `None` for any other failure.
///
/// upstream: xattrs.c:1253-1258 get_stat_xattr() (RERR_FILEIO),
/// main.c:817-821 get_local_name() (RERR_FILEIO), util2.c:73-80 my_alloc()
/// (RERR_MALLOC), and every RERR_PROTOCOL refusal, e.g. exclude.c:1924-1926
#[must_use]
pub fn cause_line_exit_code(error: &io::Error) -> Option<ExitCode> {
    if metadata::is_corrupt_fake_super(error) || DestRootMkdirError::of(error).is_some() {
        Some(ExitCode::FileIo)
    } else if error
        .get_ref()
        .is_some_and(|inner| inner.is::<protocol::MallocFailure>())
    {
        Some(ExitCode::Malloc)
    } else if error
        .get_ref()
        .is_some_and(|inner| inner.is::<protocol::ProtocolViolation>())
    {
        Some(ExitCode::Protocol)
    } else {
        None
    }
}

/// The newline-terminated lines a server reports when `error` ends its
/// transfer with `code`: the failure's own cause, when upstream prints one,
/// then the `rsync error: ... (code N) at ... [role=version]` trailer.
///
/// upstream: log.c:937-963 log_exit()
#[must_use]
pub fn fatal_report_lines(
    error: &io::Error,
    code: ExitCode,
    role: Role,
    brand: Brand,
) -> Vec<Vec<u8>> {
    let mut lines = Vec::with_capacity(2);
    let mut role = role;
    let bare = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<protocol::ProtocolViolation>())
        .is_some_and(|violation| violation.cause_line().is_none());
    if let Some(failed) = DestRootMkdirError::of(error) {
        // upstream: main.c:1248 - do_server_recv() runs get_local_name() before
        // do_recv() forks, so who_am_i() names neither half (rsync.c:987-995)
        // and rsyserr() prefixes the program name (log.c:450-468).
        role = Role::PreForkReceiver;
        lines.push(
            format!(
                "{}: [{role}] mkdir {} failed: {}\n",
                brand.client_program_name(),
                failed.fname(),
                upstream_io_error(failed.source_error())
            )
            .into_bytes(),
        );
    } else if cause_line_exit_code(error).is_some() && !bare {
        lines.push(format!("{error}\n").into_bytes());
    }
    let trailer = crate::rsync_error!(code.as_i32(), "{}", code.description())
        .with_role(role)
        .with_brand(brand);
    if let Ok(line) = trailer.to_line_bytes() {
        lines.push(line);
    }
    lines
}

/// Whether a forked receiver child exiting with `code` leaves its report
/// unsent, so its generator meets end-of-file on the error pipe instead.
///
/// upstream: cleanup.c:241-256 - only a code other than `RERR_SOCKETIO`,
/// `RERR_STREAMIO`, `RERR_SIGNAL1` and `RERR_TIMEOUT` sends `MSG_ERROR_EXIT`
/// and flushes the queued messages (`io_flush(MSG_FLUSH)`).
#[must_use]
pub const fn receiver_exit_is_unannounced(code: ExitCode) -> bool {
    matches!(
        code,
        ExitCode::SocketIo | ExitCode::StreamIo | ExitCode::Signal1 | ExitCode::Timeout
    )
}

/// The newline-terminated lines a server's generator reports when its forked
/// receiver dies unannounced: the end-of-file whine naming the generator's
/// own read count, then its `RERR_STREAMIO` trailer.
///
/// upstream: io.c:299-303 whine_about_eof(), log.c:937-963 log_exit()
#[must_use]
pub fn generator_eof_report_lines(total_read: u64, brand: Brand) -> Vec<Vec<u8>> {
    let mut lines = vec![
        format!(
            "{}: connection unexpectedly closed ({total_read} bytes received so far) [{}]\n",
            brand.client_program_name(),
            Role::Generator.as_str()
        )
        .into_bytes(),
    ];
    let code = ExitCode::StreamIo;
    let trailer = crate::rsync_error!(code.as_i32(), "{}", code.description())
        .with_role(Role::Generator)
        .with_brand(brand);
    if let Ok(line) = trailer.to_line_bytes() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare exit_cleanup(RERR_PROTOCOL) prints the code-2 trailer alone
    /// (sender.c:409-411).
    #[test]
    fn a_bare_protocol_exit_reports_the_trailer_alone() {
        let lines = fatal_report_lines(
            &protocol::bare_protocol_exit(),
            ExitCode::Protocol,
            Role::Sender,
            Brand::Upstream,
        );
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].starts_with(b"rsync error: protocol incompatibility (code 2) at "),
            "{}",
            String::from_utf8_lossy(&lines[0])
        );
    }

    /// A failure upstream prints a cause for is reported with that cause
    /// before the trailer; any other failure gets the trailer alone.
    #[test]
    fn the_cause_precedes_the_trailer_only_when_upstream_prints_one() {
        let refused = protocol::malloc_failure("[sender] exceeded --max-alloc=1048576 setting");
        let lines = fatal_report_lines(&refused, ExitCode::Malloc, Role::Sender, Brand::Upstream);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"[sender] exceeded --max-alloc=1048576 setting\n");
        let trailer = String::from_utf8(lines[1].clone()).expect("utf-8");
        assert!(
            trailer.starts_with("rsync error: error allocating core memory buffers (code 22) at "),
            "{trailer}"
        );
        assert!(trailer.contains("[sender="), "{trailer}");
        assert!(trailer.ends_with('\n'));

        let plain = io::Error::other("boom");
        let lines = fatal_report_lines(
            &plain,
            ExitCode::PartialTransfer,
            Role::Sender,
            Brand::Upstream,
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(b"rsync error: "));
    }

    /// A server receiver that cannot create the destination root prints
    /// rsyserr()'s line and a trailer naming the unforked receiver, and exits
    /// RERR_FILEIO even for the EACCES its ErrorKind would grade 3.
    ///
    /// upstream: main.c:817-821, main.c:1248, rsync.c:987-995
    #[test]
    fn a_dest_root_mkdir_failure_reports_as_the_unforked_receiver() {
        // The errno a failed mkdir reports for a read-only parent.
        #[cfg(unix)]
        let (raw, cause) = (13, "Permission denied (13)");
        // Windows reports ERROR_ACCESS_DENIED. 3.5.1 under Cygwin prints
        // "Permission denied (13)"; task 630 changes this expectation.
        #[cfg(not(unix))]
        let (raw, cause) = (5, "Access is denied. (5)");
        let failed = DestRootMkdirError::into_io(
            "\"/srv/ro/new\"".to_owned(),
            io::Error::from_raw_os_error(raw),
        );
        assert_eq!(cause_line_exit_code(&failed), Some(ExitCode::FileIo));
        let lines = fatal_report_lines(&failed, ExitCode::FileIo, Role::Generator, Brand::Upstream);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            String::from_utf8(lines[0].clone()).expect("utf-8"),
            format!("rsync: [Receiver] mkdir \"/srv/ro/new\" failed: {cause}\n")
        );
        let trailer = String::from_utf8(lines[1].clone()).expect("utf-8");
        assert!(
            trailer.starts_with("rsync error: error in file IO (code 11) at "),
            "{trailer}"
        );
        assert!(trailer.contains("[Receiver="), "{trailer}");
    }
}
