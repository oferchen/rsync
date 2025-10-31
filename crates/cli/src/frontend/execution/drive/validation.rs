use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use rsync_core::{message::Role, rsync_error};
use rsync_logging::MessageSink;
use rsync_protocol::ProtocolVersion;

use crate::frontend::write_message;

pub(super) fn validate_local_only_options<Err: Write>(
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
        let message = rsync_error!(
            1,
            "the --rsync-path option may only be used with remote connections"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "the --rsync-path option may only be used with remote connections"
            );
        }
        return Some(1);
    }

    if !remote_options.is_empty() {
        let message = rsync_error!(
            1,
            "the --remote-option option may only be used with remote connections"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "the --remote-option option may only be used with remote connections"
            );
        }
        return Some(1);
    }

    if desired_protocol.is_some() {
        let message = rsync_error!(
            1,
            "the --protocol option may only be used when accessing an rsync daemon"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "the --protocol option may only be used when accessing an rsync daemon"
            );
        }
        return Some(1);
    }

    if password_file.is_some() {
        let message = rsync_error!(
            1,
            "the --password-file option may only be used when accessing an rsync daemon"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "the --password-file option may only be used when accessing an rsync daemon"
            );
        }
        return Some(1);
    }

    if connect_program.is_some() {
        let message = rsync_error!(
            1,
            "the --connect-program option may only be used when accessing an rsync daemon"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "the --connect-program option may only be used when accessing an rsync daemon"
            );
        }
        return Some(1);
    }

    None
}
