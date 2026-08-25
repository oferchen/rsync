use std::ffi::OsString;
use std::io::Write;

use core::{message::Role, rsync_error};
use logging_sink::MessageSink;

use super::messages::fail_with_message;

/// Error message when a password option is used without a daemon connection.
pub(crate) const PASSWORD_DAEMON_ONLY_MESSAGE: &str = "the --password-file and --password-command options may only be used when accessing an rsync daemon";
/// Error message when `--connect-program` is used without a daemon connection.
pub(crate) const CONNECT_PROGRAM_DAEMON_ONLY_MESSAGE: &str =
    "the --connect-program option may only be used when accessing an rsync daemon";

/// Rejects options that are only valid for remote or daemon transfers.
///
/// Returns `Some(exit_code)` if a forbidden option was detected, `None` otherwise.
///
/// Note: `--rsync-path` is intentionally NOT rejected here. Upstream rsync
/// silently ignores it on local copies (options.c stores the value but only
/// uses it when spawning a remote shell). The upstream testsuite relies on
/// this behavior (e.g., the exclude test passes `--rsync-path` on local runs).
///
/// Note: `--remote-option` is likewise NOT rejected. Upstream appends its
/// values to the argv of the server it starts (options.c:3175-3182), and a
/// local copy still forks one, so `-M` reaches the receiving side there
/// instead of being an error. oc reproduces that by folding the values into
/// its own option stream when the transfer is local - see
/// `local_remote_option_argv` in the argument parser - so by the time this runs
/// they have already been applied.
pub(super) fn validate_local_only_options<Err>(
    has_password_override: bool,
    has_password_option: bool,
    connect_program: Option<&OsString>,
    _rsync_path: Option<&OsString>,
    stderr: &mut MessageSink<Err>,
) -> Option<i32>
where
    Err: Write,
{
    // upstream imposes no daemon-only restriction on `--protocol`: setup_protocol
    // (compat.c) runs for local copies too, so `--protocol=N` (20..=32) is
    // accepted locally and simply ignored (this build never negotiates a
    // protocol for a local copy). See resolve_desired_protocol.

    if has_password_override || has_password_option {
        return Some(reject_local_only_option(
            stderr,
            PASSWORD_DAEMON_ONLY_MESSAGE,
        ));
    }

    if connect_program.is_some() {
        return Some(reject_local_only_option(
            stderr,
            CONNECT_PROGRAM_DAEMON_ONLY_MESSAGE,
        ));
    }

    None
}

/// Emits an error for a local-only option violation and returns the exit code.
fn reject_local_only_option<Err>(stderr: &mut MessageSink<Err>, text: &'static str) -> i32
where
    Err: Write,
{
    let message = rsync_error!(1, "{}", text).with_role(Role::Client);
    fail_with_message(message, stderr)
}
