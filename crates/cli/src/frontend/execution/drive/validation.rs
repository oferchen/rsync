use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use core::{message::Role, rsync_error};
use logging_sink::MessageSink;
use protocol::ProtocolVersion;

use super::messages::fail_with_message;

pub(crate) const RSYNC_PATH_REMOTE_ONLY_MESSAGE: &str =
    "the --rsync-path option may only be used with remote connections";
pub(crate) const REMOTE_OPTION_REMOTE_ONLY_MESSAGE: &str =
    "the --remote-option option may only be used with remote connections";
pub(crate) const PROTOCOL_DAEMON_ONLY_MESSAGE: &str =
    "the --protocol option may only be used when accessing an rsync daemon";
pub(crate) const PASSWORD_FILE_DAEMON_ONLY_MESSAGE: &str =
    "the --password-file option may only be used when accessing an rsync daemon";
pub(crate) const CONNECT_PROGRAM_DAEMON_ONLY_MESSAGE: &str =
    "the --connect-program option may only be used when accessing an rsync daemon";

pub(super) fn validate_local_only_options<Err>(
    desired_protocol: Option<ProtocolVersion>,
    password_file: Option<&PathBuf>,
    connect_program: Option<&OsString>,
    rsync_path: Option<&OsString>,
    remote_options: &[OsString],
    stderr: &mut MessageSink<Err>,
) -> Option<i32>
where
    Err: Write,
{
    if rsync_path.is_some() {
        return Some(reject_local_only_option(
            stderr,
            RSYNC_PATH_REMOTE_ONLY_MESSAGE,
        ));
    }

    if !remote_options.is_empty() {
        return Some(reject_local_only_option(
            stderr,
            REMOTE_OPTION_REMOTE_ONLY_MESSAGE,
        ));
    }

    if desired_protocol.is_some() {
        return Some(reject_local_only_option(
            stderr,
            PROTOCOL_DAEMON_ONLY_MESSAGE,
        ));
    }

    if password_file.is_some() {
        return Some(reject_local_only_option(
            stderr,
            PASSWORD_FILE_DAEMON_ONLY_MESSAGE,
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

fn reject_local_only_option<Err>(stderr: &mut MessageSink<Err>, text: &'static str) -> i32
where
    Err: Write,
{
    let message = rsync_error!(1, "{}", text).with_role(Role::Client);
    fail_with_message(message, stderr)
}
