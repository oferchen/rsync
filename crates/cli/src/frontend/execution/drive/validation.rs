use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use rsync_core::{message::Role, rsync_error};
use rsync_logging::MessageSink;
use rsync_protocol::ProtocolVersion;

use crate::frontend::write_message;

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
    fallback_required: bool,
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
    if fallback_required {
        return None;
    }

    if rsync_path.is_some() {
        let message = rsync_error!(1, "{}", RSYNC_PATH_REMOTE_ONLY_MESSAGE).with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(stderr.writer_mut(), "{}", RSYNC_PATH_REMOTE_ONLY_MESSAGE);
        }
        return Some(1);
    }

    if !remote_options.is_empty() {
        let message =
            rsync_error!(1, "{}", REMOTE_OPTION_REMOTE_ONLY_MESSAGE).with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(stderr.writer_mut(), "{}", REMOTE_OPTION_REMOTE_ONLY_MESSAGE);
        }
        return Some(1);
    }

    if desired_protocol.is_some() {
        let message = rsync_error!(1, "{}", PROTOCOL_DAEMON_ONLY_MESSAGE).with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(stderr.writer_mut(), "{}", PROTOCOL_DAEMON_ONLY_MESSAGE);
        }
        return Some(1);
    }

    if password_file.is_some() {
        let message =
            rsync_error!(1, "{}", PASSWORD_FILE_DAEMON_ONLY_MESSAGE).with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(stderr.writer_mut(), "{}", PASSWORD_FILE_DAEMON_ONLY_MESSAGE);
        }
        return Some(1);
    }

    if connect_program.is_some() {
        let message =
            rsync_error!(1, "{}", CONNECT_PROGRAM_DAEMON_ONLY_MESSAGE).with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "{}",
                CONNECT_PROGRAM_DAEMON_ONLY_MESSAGE
            );
        }
        return Some(1);
    }

    None
}
