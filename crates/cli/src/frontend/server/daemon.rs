//! Daemon mode detection and dispatch.

use std::ffi::OsString;
use std::io::Write;

use core::branding::Brand;

/// Returns the daemon argument vector when `--daemon` is present.
pub(crate) fn daemon_mode_arguments(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.is_empty() {
        return None;
    }

    let program_name = super::super::detect_program_name(args.first().map(OsString::as_os_str));
    let daemon_program = match program_name {
        super::super::ProgramName::Rsync => Brand::Upstream.daemon_program_name(),
        super::super::ProgramName::OcRsync => Brand::Oc.daemon_program_name(),
    };

    let mut daemon_args = Vec::with_capacity(args.len());
    daemon_args.push(OsString::from(daemon_program));

    let mut found = false;
    let mut reached_double_dash = false;

    for arg in args.iter().skip(1) {
        if !reached_double_dash && arg == "--" {
            reached_double_dash = true;
            daemon_args.push(arg.clone());
            continue;
        }

        if !reached_double_dash && arg == "--daemon" {
            found = true;
            continue;
        }

        daemon_args.push(arg.clone());
    }

    if found { Some(daemon_args) } else { None }
}

/// Returns `true` when the invocation requests server mode.
///
/// Only considers `--server` appearing before any `--` separator.
pub(crate) fn server_mode_requested(args: &[OsString]) -> bool {
    for arg in args.iter().skip(1) {
        if arg == "--" {
            return false;
        }
        if arg == "--server" {
            return true;
        }
    }
    false
}

/// Returns `true` when the invocation requests remote-shell daemon mode.
///
/// This is the `--server --daemon` combination where rsync serves the daemon
/// protocol over stdin/stdout instead of binding a TCP listener. Used by
/// remote shell wrappers (e.g., `lsh.sh`) that invoke rsync as:
///   `rsync --config=<file> --server --daemon .`
///
/// upstream: main.c:1867-1868 - when both `am_server` and `am_daemon` are set,
/// `start_daemon(STDIN_FILENO, STDOUT_FILENO)` is called.
pub(crate) fn server_daemon_mode_requested(args: &[OsString]) -> bool {
    let mut has_server = false;
    let mut has_daemon = false;
    for arg in args.iter().skip(1) {
        if arg == "--" {
            break;
        }
        if arg == "--server" {
            has_server = true;
        }
        if arg == "--daemon" {
            has_daemon = true;
        }
    }
    has_server && has_daemon
}

/// Extracts daemon arguments for the `--server --daemon` stdio path.
///
/// Strips `--server` and `--daemon` from the argument list, retaining any
/// `--config=<path>` or other daemon-relevant options. The returned vector
/// is suitable for passing to `DaemonConfig::builder().arguments(...)`.
#[cfg(any(unix, test))]
pub(crate) fn server_daemon_arguments(args: &[OsString]) -> Vec<OsString> {
    let program_name = super::super::detect_program_name(args.first().map(OsString::as_os_str));
    let daemon_program = match program_name {
        super::super::ProgramName::Rsync => Brand::Upstream.daemon_program_name(),
        super::super::ProgramName::OcRsync => Brand::Oc.daemon_program_name(),
    };

    let mut daemon_args = Vec::with_capacity(args.len());
    daemon_args.push(OsString::from(daemon_program));

    for arg in args.iter().skip(1) {
        if arg == "--server" || arg == "--daemon" || arg == "." {
            continue;
        }
        daemon_args.push(arg.clone());
    }

    daemon_args
}

/// Delegates execution to the daemon front-end (Unix) or reports that daemon
/// mode is unavailable (Windows).
#[cfg(unix)]
pub(crate) fn run_daemon_mode<Out, Err>(
    args: Vec<OsString>,
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    daemon::run(args, stdout, stderr)
}

/// Runs the daemon protocol over stdin/stdout for remote-shell daemon mode.
///
/// This is the `--server --daemon` path where the daemon protocol runs over
/// inherited file descriptors. Used by remote shells (e.g., SSH) that invoke:
///   `oc-rsync --config=<file> --server --daemon .`
///
/// upstream: main.c:1867-1868 - `start_daemon(STDIN_FILENO, STDOUT_FILENO)`.
#[cfg(unix)]
pub(crate) fn run_server_daemon_mode<Err>(args: &[OsString], stderr: &mut Err) -> i32
where
    Err: Write,
{
    use core::message::Role;
    use core::rsync_error;
    use daemon::{DaemonConfig, run_daemon_stdio};
    use logging_sink::MessageSink;

    let program_brand =
        super::super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    let daemon_args = server_daemon_arguments(args);

    let config = DaemonConfig::builder()
        .brand(program_brand)
        .arguments(daemon_args.iter().skip(1).cloned())
        .build();

    match run_daemon_stdio(config) {
        Ok(()) => 0,
        Err(error) => {
            let mut sink = MessageSink::with_brand(stderr, program_brand);
            let mut message = rsync_error!(error.exit_code(), format!("{error}"));
            message = message.with_role(Role::Daemon);
            if super::super::write_message(&message, &mut sink).is_err() {
                let _ = writeln!(sink.writer_mut(), "{error}");
            }
            error.exit_code()
        }
    }
}

/// Reports that server-daemon mode is unavailable on Windows.
#[cfg(windows)]
pub(crate) fn run_server_daemon_mode<Err>(args: &[OsString], stderr: &mut Err) -> i32
where
    Err: Write,
{
    let program_brand =
        super::super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    write_daemon_unavailable_error(stderr, program_brand);
    1
}

/// Reports that daemon mode is unavailable on Windows.
#[cfg(windows)]
pub(crate) fn run_daemon_mode<Out, Err>(
    args: Vec<OsString>,
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let program_brand =
        super::super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    write_daemon_unavailable_error(stderr, program_brand);
    1
}

#[cfg(windows)]
fn write_daemon_unavailable_error<Err: Write>(stderr: &mut Err, brand: Brand) {
    use core::message::Role;
    use core::rsync_error;
    use logging_sink::MessageSink;

    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(
        1,
        "daemon mode is not supported on this platform; run the oc-rsync daemon on a Unix-like system"
    );
    message = message.with_role(Role::Client);

    if super::super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(
            sink.writer_mut(),
            "daemon mode is not supported on this platform; run the oc-rsync daemon on a Unix-like system"
        );
    }
}
