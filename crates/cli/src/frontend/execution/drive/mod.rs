mod config;
mod fallback;
mod filters;
mod messages;
mod metadata;
mod module_listing;
mod options;
mod summary;
mod validation;
#[cfg(test)]
pub(crate) use validation::CONNECT_PROGRAM_DAEMON_ONLY_MESSAGE;
mod workflow;

use rsync_logging::MessageSink;
use std::io::Write;

pub(crate) use workflow::execute;

#[cfg(test)]
use super::super::arguments::ProgramName;

#[cfg(test)]
pub(crate) fn render_missing_operands_stdout(program_name: ProgramName) -> String {
    workflow::render_missing_operands_stdout(program_name)
}

pub(crate) fn with_output_writer<'a, Out, Err, R>(
    stdout: &'a mut Out,
    stderr: &'a mut MessageSink<Err>,
    use_stderr: bool,
    f: impl FnOnce(&'a mut dyn Write) -> R,
) -> R
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    if use_stderr {
        let writer: &mut Err = stderr.writer_mut();
        f(writer)
    } else {
        f(stdout)
    }
}
