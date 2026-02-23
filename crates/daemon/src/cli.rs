use std::ffi::OsString;
use std::io::Write;

use core::{
    branding::{self},
    message::Role,
    rsync_error,
    version::VersionInfoReport,
};
use logging_sink::MessageSink;

use crate::{
    config::DaemonConfig,
    daemon::{MAX_EXIT_CODE, ParsedArgs, parse_args, render_help, run_daemon, write_message},
};

/// Runs the daemon CLI using the provided argument iterator and output handles.
///
/// The function returns the process exit code that should be used by the caller.
/// Diagnostics are rendered using the central [`core::message`] utilities.
#[allow(clippy::module_name_repetitions)]
pub fn run<I, S, Out, Err>(arguments: I, stdout: &mut Out, stderr: &mut Err) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    let args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();
    let brand = branding::detect_brand(args.first().map(OsString::as_os_str));
    let mut stderr_sink = MessageSink::with_brand(stderr, brand);
    match parse_args(args) {
        Ok(parsed) => execute(parsed, stdout, &mut stderr_sink),
        Err(error) => {
            let message = rsync_error!(1, "{}", error).with_role(Role::Daemon);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{error}");
            }
            1
        }
    }
}

fn execute<Out, Err>(parsed: ParsedArgs, stdout: &mut Out, stderr: &mut MessageSink<Err>) -> i32
where
    Out: Write,
    Err: Write,
{
    // 1) handle help/version fast-paths
    if parsed.show_help {
        let help = render_help(parsed.program_name);
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if parsed.show_version && parsed.remainder.is_empty() {
        let report = VersionInfoReport::for_daemon_brand(parsed.program_name.brand());
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    // 2) run native daemon mode
    let config = DaemonConfig::builder()
        .brand(parsed.program_name.brand())
        .arguments(parsed.remainder)
        .build();

    match run_daemon(config) {
        Ok(()) => 0,
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let message = error.message();
                let _ = writeln!(stderr.writer_mut(), "{message}");
            }
            error.exit_code()
        }
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_from_clamps_values() {
        assert_eq!(exit_code_from(-1), std::process::ExitCode::from(0));
        assert_eq!(exit_code_from(42), std::process::ExitCode::from(42));
        assert_eq!(exit_code_from(9_999), std::process::ExitCode::from(u8::MAX));
    }

    #[test]
    fn exit_code_from_handles_boundary_values() {
        assert_eq!(exit_code_from(0), std::process::ExitCode::from(0));
        assert_eq!(exit_code_from(255), std::process::ExitCode::from(255));

        assert_eq!(exit_code_from(-100), std::process::ExitCode::from(0));
        assert_eq!(exit_code_from(i32::MIN), std::process::ExitCode::from(0));

        assert_eq!(exit_code_from(256), std::process::ExitCode::from(255));
        assert_eq!(exit_code_from(1000), std::process::ExitCode::from(255));
        assert_eq!(exit_code_from(i32::MAX), std::process::ExitCode::from(255));
    }

    #[test]
    fn run_with_help_flag_returns_zero() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run(["oc-rsyncd", "--help"], &mut stdout, &mut stderr);
        assert_eq!(result, 0);
        assert!(!stdout.is_empty());
    }

    #[test]
    fn run_with_version_flag_returns_zero() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run(["oc-rsyncd", "--version"], &mut stdout, &mut stderr);
        assert_eq!(result, 0);
        assert!(!stdout.is_empty());
    }
}
