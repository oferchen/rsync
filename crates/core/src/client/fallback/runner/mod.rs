use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use super::super::{ClientError, MAX_EXIT_CODE};
use super::args::RemoteFallbackArgs;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

mod command_builder;
pub(crate) mod helpers;

use command_builder::{PreparedInvocation, prepare_invocation};
use helpers::{
    FallbackStreamKind, FallbackStreamMessage, fallback_error, join_fallback_thread,
    spawn_fallback_reader, terminate_fallback_process, write_daemon_password,
};

/// Spawns the fallback `rsync` binary with arguments derived from [`RemoteFallbackArgs`].
///
/// The helper forwards the subprocess stdout/stderr into the provided writers and returns
/// the exit status code on success. Errors surface as [`ClientError`] instances with
/// fully formatted diagnostics.
pub fn run_remote_transfer_fallback<Out, Err>(
    stdout: &mut Out,
    stderr: &mut Err,
    args: RemoteFallbackArgs,
) -> Result<i32, ClientError>
where
    Out: Write,
    Err: Write,
{
    let PreparedInvocation {
        binary,
        args: command_args,
        mut daemon_password,
        files_from_temp,
    } = prepare_invocation(args)?;

    let mut command = Command::new(&binary);
    command.args(&command_args);
    if daemon_password.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::inherit());
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        fallback_error(format!(
            "failed to launch fallback rsync binary '{}': {error}",
            Path::new(&binary).display()
        ))
    })?;

    if let Some(mut password) = daemon_password.take() {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| fallback_error("fallback rsync did not expose a writable stdin"))?;

        write_daemon_password(&mut stdin, &mut password).map_err(|error| {
            fallback_error(format!(
                "failed to write password to fallback rsync stdin: {error}"
            ))
        })?;
    }

    let (sender, receiver) = mpsc::channel();
    let mut stdout_thread = child
        .stdout
        .take()
        .map(|handle| spawn_fallback_reader(handle, FallbackStreamKind::Stdout, sender.clone()));
    let mut stderr_thread = child
        .stderr
        .take()
        .map(|handle| spawn_fallback_reader(handle, FallbackStreamKind::Stderr, sender.clone()));
    drop(sender);

    let mut stdout_open = stdout_thread.is_some();
    let mut stderr_open = stderr_thread.is_some();

    while stdout_open || stderr_open {
        match receiver.recv() {
            Ok(FallbackStreamMessage::Data(FallbackStreamKind::Stdout, data)) => {
                if let Err(error) = stdout.write_all(&data) {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(format!(
                        "failed to forward fallback stdout: {error}"
                    )));
                }
            }
            Ok(FallbackStreamMessage::Data(FallbackStreamKind::Stderr, data)) => {
                if let Err(error) = stderr.write_all(&data) {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(format!(
                        "failed to forward fallback stderr: {error}"
                    )));
                }
            }
            Ok(FallbackStreamMessage::Error(FallbackStreamKind::Stdout, error)) => {
                terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                return Err(fallback_error(format!(
                    "failed to read stdout from fallback rsync: {error}"
                )));
            }
            Ok(FallbackStreamMessage::Error(FallbackStreamKind::Stderr, error)) => {
                terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                return Err(fallback_error(format!(
                    "failed to read stderr from fallback rsync: {error}"
                )));
            }
            Ok(FallbackStreamMessage::Finished(kind)) => match kind {
                FallbackStreamKind::Stdout => stdout_open = false,
                FallbackStreamKind::Stderr => stderr_open = false,
            },
            Err(_) => {
                if stdout_open {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(
                        "failed to capture stdout from fallback rsync binary",
                    ));
                }
                if stderr_open {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(
                        "failed to capture stderr from fallback rsync binary",
                    ));
                }
                break;
            }
        }
    }

    join_fallback_thread(&mut stdout_thread);
    join_fallback_thread(&mut stderr_thread);

    let status = child.wait().map_err(|error| {
        fallback_error(format!(
            "failed to wait for fallback rsync process: {error}"
        ))
    })?;

    drop(files_from_temp);

    let exit_code = match status.code() {
        Some(code) => code,
        None => {
            #[cfg(unix)]
            {
                if let Some(signal) = status.signal() {
                    return Ok((128 + signal).min(MAX_EXIT_CODE));
                }
            }

            MAX_EXIT_CODE
        }
    };

    Ok(exit_code)
}
