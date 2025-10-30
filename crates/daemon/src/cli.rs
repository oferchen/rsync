use std::env;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};

use rsync_core::{
    branding::Brand, fallback::DAEMON_AUTO_DELEGATE_ENV, message::Role, rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;

use super::{
    DaemonConfig, MAX_EXIT_CODE, ParsedArgs, configured_fallback_binary, parse_args, render_help,
    run_daemon, write_message,
};

/// Runs the daemon CLI using the provided argument iterator and output handles.
///
/// The function returns the process exit code that should be used by the caller.
/// Diagnostics are rendered using the central [`rsync_core::message`] utilities.
#[allow(clippy::module_name_repetitions)]
pub fn run<I, S, Out, Err>(arguments: I, stdout: &mut Out, stderr: &mut Err) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    let mut stderr_sink = MessageSink::new(stderr);
    match parse_args(arguments) {
        Ok(parsed) => execute(parsed, stdout, &mut stderr_sink),
        Err(error) => {
            let mut message = rsync_error!(1, "{}", error);
            message = message.with_role(Role::Daemon);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{}", error);
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

    if parsed.delegate_system_rsync
        || auto_delegate_system_rsync_enabled()
        || fallback_binary_configured()
    {
        return run_delegate_mode(parsed.remainder.as_slice(), stderr);
    }

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .brand(parsed.program_name.brand())
        .arguments(parsed.remainder)
        .build();

    match run_daemon(config) {
        Ok(()) => 0,
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let _ = writeln!(stderr.writer_mut(), "{}", error.message());
            }
            error.exit_code()
        }
    }
}

fn auto_delegate_system_rsync_enabled() -> bool {
    matches!(env_flag(DAEMON_AUTO_DELEGATE_ENV), Some(true))
}

pub(super) fn fallback_binary_configured() -> bool {
    configured_fallback_binary().is_some()
}

fn fallback_binary() -> OsString {
    configured_fallback_binary()
        .unwrap_or_else(|| OsString::from(Brand::Upstream.client_program_name()))
}

fn env_flag(name: &str) -> Option<bool> {
    let value = env::var_os(name)?;
    let value = value.to_string_lossy();
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Some(true);
    }

    if trimmed.eq_ignore_ascii_case("0")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("off")
    {
        Some(false)
    } else {
        Some(true)
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

fn run_delegate_mode<Err>(args: &[OsString], stderr: &mut MessageSink<Err>) -> i32
where
    Err: Write,
{
    let binary = fallback_binary();

    let mut command = ProcessCommand::new(&binary);
    command.arg("--daemon");
    command.arg("--no-detach");
    command.args(args);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = rsync_error!(
                1,
                format!(
                    "failed to launch system rsync daemon '{}': {}",
                    Path::new(&binary).display(),
                    error
                )
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to launch system rsync daemon '{}': {}",
                    Path::new(&binary).display(),
                    error
                );
            }
            return 1;
        }
    };

    match child.wait() {
        Ok(status) => {
            if status.success() {
                0
            } else {
                let code = status.code().unwrap_or(MAX_EXIT_CODE);
                let message = rsync_error!(
                    code,
                    format!("system rsync daemon exited with status {}", status)
                )
                .with_role(Role::Daemon);
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(
                        stderr.writer_mut(),
                        "system rsync daemon exited with status {}",
                        status
                    );
                }
                code
            }
        }
        Err(error) => {
            let message = rsync_error!(
                1,
                format!("failed to wait for system rsync daemon: {}", error)
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to wait for system rsync daemon: {}",
                    error
                );
            }
            1
        }
    }
}
