#![deny(unsafe_code)]

use crate::frontend::arguments::ProgramName;
use oc_rsync_core::{message::Role, rsync_exit_code, version::VersionInfoReport};
use oc_rsync_logging::MessageSink;
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

pub(crate) fn render_missing_operands_stdout(program_name: ProgramName) -> String {
    let mut rendered = VersionInfoReport::for_client_brand(program_name.brand()).human_readable();
    rendered.push('\n');
    rendered.push_str(&render_missing_operands_synopsis(program_name));
    rendered
}

fn render_missing_operands_synopsis(program_name: ProgramName) -> String {
    let program = program_name.as_str();
    format!(
        concat!(
            "{program} is a file transfer program capable of efficient remote update\n",
            "via a fast differencing algorithm.\n",
            "\n",
            "Usage: {program} [OPTION]... SRC [SRC]... DEST\n",
            "  or   {program} [OPTION]... SRC [SRC]... [USER@]HOST:DEST\n",
            "  or   {program} [OPTION]... SRC [SRC]... [USER@]HOST::DEST\n",
            "  or   {program} [OPTION]... SRC [SRC]... rsync://[USER@]HOST[:PORT]/DEST\n",
            "  or   {program} [OPTION]... [USER@]HOST:SRC [DEST]\n",
            "  or   {program} [OPTION]... [USER@]HOST::SRC [DEST]\n",
            "  or   {program} [OPTION]... rsync://[USER@]HOST[:PORT]/SRC [DEST]\n",
            "The ':' usages connect via remote shell, while '::' & 'rsync://' usages connect\n",
            "to an rsync daemon, and require SRC or DEST to start with a module name.\n",
        ),
        program = program,
    )
}
