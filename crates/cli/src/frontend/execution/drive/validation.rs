use std::ffi::OsString;
use std::io::Write;

use core::{message::Role, rsync_error};
use logging_sink::MessageSink;

use super::messages::fail_with_message;

/// Error message when `--remote-option` is used without a remote connection.
pub(crate) const REMOTE_OPTION_REMOTE_ONLY_MESSAGE: &str =
    "the --remote-option option may only be used with remote connections";
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
pub(super) fn validate_local_only_options<Err>(
    has_password_override: bool,
    has_password_option: bool,
    connect_program: Option<&OsString>,
    _rsync_path: Option<&OsString>,
    remote_options: &[OsString],
    stderr: &mut MessageSink<Err>,
) -> Option<i32>
where
    Err: Write,
{
    if !remote_options.is_empty() {
        return Some(reject_local_only_option(
            stderr,
            REMOTE_OPTION_REMOTE_ONLY_MESSAGE,
        ));
    }

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
