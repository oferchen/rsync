#![deny(unsafe_code)]

use crate::frontend::{arguments::ProgramName, command_builder::clap_command};
use rsync_core::{message::Role, rsync_error};
use rsync_logging::MessageSink;
use std::ffi::OsString;
use std::io::Write;

use super::super::messages::fail_with_message;

pub(crate) fn ensure_transfer_operands_present<Out, Err>(
    transfer_operands: &[OsString],
    program_name: ProgramName,
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
) -> Result<(), i32>
where
    Out: Write,
    Err: Write,
{
    if transfer_operands.is_empty() {
        let usage = clap_command(program_name.as_str())
            .render_usage()
            .to_string();
        if writeln!(stdout, "{usage}").is_err() {
            let _ = writeln!(stderr.writer_mut(), "{usage}");
        }

        let message = rsync_error!(
            1,
            "missing source operands: supply at least one source and a destination"
        )
        .with_role(Role::Client);
        Err(fail_with_message(message, stderr))
    } else {
        Ok(())
    }
}
