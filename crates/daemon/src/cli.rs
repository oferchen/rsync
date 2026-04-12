//! CLI entry points for the rsync daemon binary.
//!
//! Provides the top-level [`run`] function that parses command-line arguments,
//! dispatches to `--help`/`--version` fast paths, or delegates to
//! [`run_daemon`] for full daemon mode. Diagnostics are routed through the
//! central [`core::message`] system so output formatting and exit codes match
//! upstream rsync behaviour.
//!
//! upstream: main.c - the daemon CLI is a mode of the main `rsync` binary,
//! triggered by `--daemon` on the command line.

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
    daemon::{
        MAX_EXIT_CODE, ParsedArgs, ServiceAction, parse_args, render_help, run_daemon,
        write_message,
    },
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

    // Handle Windows Service management actions before entering the daemon loop.
    if let Some(action) = parsed.service_action {
        return execute_service_action(action, &parsed, stdout, stderr);
    }

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

/// Executes a Windows Service management action (install, uninstall, or run as service).
///
/// On non-Windows platforms, these actions return a descriptive error. On Windows,
/// they delegate to `platform::windows_service` for SCM integration.
fn execute_service_action<Out, Err>(
    action: ServiceAction,
    parsed: &ParsedArgs,
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let result = match action {
        ServiceAction::Install => platform::windows_service::install_service().map(|()| {
            let _ = writeln!(
                stdout,
                "Service '{}' installed successfully.",
                platform::windows_service::SERVICE_NAME
            );
        }),
        ServiceAction::Uninstall => platform::windows_service::uninstall_service().map(|()| {
            let _ = writeln!(
                stdout,
                "Service '{}' removed successfully.",
                platform::windows_service::SERVICE_NAME
            );
        }),
        ServiceAction::RunAsService => {
            let remainder = parsed.remainder.clone();
            let brand = parsed.program_name.brand();
            platform::windows_service::run_service_dispatcher(Box::new(move |flags| {
                let config = DaemonConfig::builder()
                    .brand(brand)
                    .arguments(remainder)
                    .signal_flags(flags)
                    .build();
                run_daemon(config).map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::Other, error.message().to_string())
                })
            }))
        }
    };

    match result {
        Ok(()) => 0,
        Err(error) => {
            let message = rsync_error!(1, "{error}").with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(stderr.writer_mut(), "{error}");
            }
            1
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

    #[test]
    fn parse_args_windows_service_flag() {
        use crate::daemon::{ServiceAction, parse_args};
        let parsed = parse_args(["oc-rsyncd", "--windows-service"]).unwrap();
        assert_eq!(parsed.service_action, Some(ServiceAction::RunAsService));
    }

    #[test]
    fn parse_args_install_service_flag() {
        use crate::daemon::{ServiceAction, parse_args};
        let parsed = parse_args(["oc-rsyncd", "--install-service"]).unwrap();
        assert_eq!(parsed.service_action, Some(ServiceAction::Install));
    }

    #[test]
    fn parse_args_uninstall_service_flag() {
        use crate::daemon::{ServiceAction, parse_args};
        let parsed = parse_args(["oc-rsyncd", "--uninstall-service"]).unwrap();
        assert_eq!(parsed.service_action, Some(ServiceAction::Uninstall));
    }

    #[test]
    fn parse_args_no_service_flag() {
        use crate::daemon::parse_args;
        let parsed = parse_args(["oc-rsyncd"]).unwrap();
        assert_eq!(parsed.service_action, None);
    }

    #[cfg(not(windows))]
    #[test]
    fn install_service_returns_error_on_non_windows() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run(["oc-rsyncd", "--install-service"], &mut stdout, &mut stderr);
        assert_eq!(result, 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn uninstall_service_returns_error_on_non_windows() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run(
            ["oc-rsyncd", "--uninstall-service"],
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(result, 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_service_returns_error_on_non_windows() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run(["oc-rsyncd", "--windows-service"], &mut stdout, &mut stderr);
        assert_eq!(result, 1);
    }
}
