#![deny(unsafe_code)]

use crate::frontend::{arguments::ProgramName, render_help};
use core::{message::Role, rsync_exit_code};
use logging_sink::MessageSink;
use std::ffi::OsString;
use std::io::Write;

use super::super::messages::fail_with_message;

/// Validates that at least one transfer operand is present, printing help if not.
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
        let banner = render_missing_operands_stdout(program_name);
        if stdout.write_all(banner.as_bytes()).is_err() {
            let _ = stderr.writer_mut().write_all(banner.as_bytes());
        }

        let message = rsync_exit_code!(1)
            .expect("exit code 1 must have a canonical diagnostic")
            .with_role(Role::Client);
        Err(fail_with_message(message, stderr))
    } else {
        Ok(())
    }
}

/// Renders the help text shown when no operands are provided.
pub(crate) fn render_missing_operands_stdout(program_name: ProgramName) -> String {
    render_help(program_name)
}
