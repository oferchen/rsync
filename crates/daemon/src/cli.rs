use std::ffi::OsString;
use std::io::Write;

<<<<<<< HEAD
use core::{
    branding::{self, Brand},
    fallback::{
        CLIENT_FALLBACK_ENV, DAEMON_AUTO_DELEGATE_ENV, DAEMON_FALLBACK_ENV, FallbackOverride,
        describe_missing_fallback_binary, fallback_binary_available, fallback_override,
    },
    message::{Message, Role, strings},
    rsync_error,
    version::VersionInfoReport,
};
=======
use core::{branding, message::Role, rsync_error, version::VersionInfoReport};
>>>>>>> origin/implement-native-server-mode-in-rust
use logging::MessageSink;

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
            let message = detail_with_exit_code(1, error.to_string()).with_role(Role::Daemon);
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
        .disable_default_paths()
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

<<<<<<< HEAD
fn run_delegate_mode<Err>(args: &[OsString], stderr: &mut MessageSink<Err>) -> i32
where
    Err: Write,
{
    let binary = fallback_binary();

    if !fallback_binary_available(binary.as_os_str()) {
        let diagnostic = describe_missing_fallback_binary(
            binary.as_os_str(),
            &[DAEMON_FALLBACK_ENV, CLIENT_FALLBACK_ENV],
        );
        let message = detail_with_exit_code(1, diagnostic.clone()).with_role(Role::Daemon);
        let fallback = message.to_string();
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(stderr.writer_mut(), "{fallback}");
        }
        return 1;
    }

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
            let binary_display = Path::new(&binary).display();
            let message = detail_with_exit_code(
                1,
                format!("failed to launch system rsync daemon '{binary_display}': {error}"),
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to launch system rsync daemon '{binary_display}': {error}"
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
                let message = detail_with_exit_code(
                    code,
                    format!("system rsync daemon exited with status {status}"),
                )
                .with_role(Role::Daemon);
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(
                        stderr.writer_mut(),
                        "system rsync daemon exited with status {status}"
                    );
                }
                code
            }
        }
        Err(error) => {
            let message = detail_with_exit_code(
                1,
                format!("failed to wait for system rsync daemon: {error}"),
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to wait for system rsync daemon: {error}"
                );
            }
            1
        }
    }
}

fn detail_with_exit_code(code: i32, detail: impl Into<String>) -> Message {
    let detail = detail.into();
    strings::exit_code_message_with_detail(code, detail.clone())
        .unwrap_or_else(|| rsync_error!(code, detail))
}

=======
>>>>>>> origin/implement-native-server-mode-in-rust
#[cfg(test)]
mod tests {}
