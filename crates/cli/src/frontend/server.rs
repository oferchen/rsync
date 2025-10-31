use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use rsync_core::branding::Brand;
use rsync_core::fallback::{CLIENT_FALLBACK_ENV, FallbackOverride, fallback_override};
use rsync_core::message::Role;
use rsync_core::rsync_error;
use rsync_logging::MessageSink;

/// Returns the daemon argument vector when `--daemon` is present.
pub(crate) fn daemon_mode_arguments(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.is_empty() {
        return None;
    }

    let program_name = super::detect_program_name(args.first().map(OsString::as_os_str));
    let daemon_program = match program_name {
        super::ProgramName::Rsync => Brand::Upstream.daemon_program_name(),
        super::ProgramName::OcRsync => Brand::Oc.daemon_program_name(),
    };

    let mut daemon_args = Vec::with_capacity(args.len());
    daemon_args.push(OsString::from(daemon_program));

    let mut found = false;
    let mut reached_double_dash = false;

    for arg in args.iter().skip(1) {
        if !reached_double_dash && arg == "--" {
            reached_double_dash = true;
            daemon_args.push(arg.clone());
            continue;
        }

        if !reached_double_dash && arg == "--daemon" {
            found = true;
            continue;
        }

        daemon_args.push(arg.clone());
    }

    if found { Some(daemon_args) } else { None }
}

/// Returns `true` when the invocation requests server mode.
pub(crate) fn server_mode_requested(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|arg| arg == "--server")
}

/// Delegates execution to the daemon front-end.
pub(crate) fn run_daemon_mode<Out, Err>(
    args: Vec<OsString>,
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    rsync_daemon::run(args, stdout, stderr)
}

/// Delegates execution to the system rsync binary when `--server` is requested.
pub(crate) fn run_server_mode<Out, Err>(
    args: &[OsString],
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let upstream_program = Brand::Upstream.client_program_name();
    let upstream_program_os = OsStr::new(upstream_program);
    let fallback = match fallback_override(CLIENT_FALLBACK_ENV) {
        Some(FallbackOverride::Disabled) => {
            let text = format!(
                "remote server mode is unavailable because OC_RSYNC_FALLBACK is disabled; set OC_RSYNC_FALLBACK to point to an upstream {upstream_program} binary"
            );
            write_server_fallback_error(stderr, text);
            return 1;
        }
        Some(other) => other
            .resolve_or_default(upstream_program_os)
            .unwrap_or_else(|| OsString::from(upstream_program)),
        None => OsString::from(upstream_program),
    };

    let mut command = Command::new(&fallback);
    command.args(args.iter().skip(1));
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let text = format!(
                "failed to launch fallback {upstream_program} binary '{}': {error}",
                Path::new(&fallback).display()
            );
            write_server_fallback_error(stderr, text);
            return 1;
        }
    };

    let (sender, receiver) = mpsc::channel();
    let mut stdout_thread = child
        .stdout
        .take()
        .map(|handle| spawn_server_reader(handle, ServerStreamKind::Stdout, sender.clone()));
    let mut stderr_thread = child
        .stderr
        .take()
        .map(|handle| spawn_server_reader(handle, ServerStreamKind::Stderr, sender.clone()));
    drop(sender);

    let mut stdout_open = stdout_thread.is_some();
    let mut stderr_open = stderr_thread.is_some();

    while stdout_open || stderr_open {
        match receiver.recv() {
            Ok(ServerStreamMessage::Data(ServerStreamKind::Stdout, data)) => {
                if let Err(error) = stdout.write_all(&data) {
                    terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    write_server_fallback_error(
                        stderr,
                        format!("failed to forward fallback stdout: {error}"),
                    );
                    return 1;
                }
            }
            Ok(ServerStreamMessage::Data(ServerStreamKind::Stderr, data)) => {
                if let Err(error) = stderr.write_all(&data) {
                    terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    write_server_fallback_error(
                        stderr,
                        format!("failed to forward fallback stderr: {error}"),
                    );
                    return 1;
                }
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stdout, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_fallback_error(
                    stderr,
                    format!("failed to read stdout from fallback {upstream_program}: {error}"),
                );
                return 1;
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stderr, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_fallback_error(
                    stderr,
                    format!("failed to read stderr from fallback {upstream_program}: {error}"),
                );
                return 1;
            }
            Ok(ServerStreamMessage::Finished(kind)) => match kind {
                ServerStreamKind::Stdout => stdout_open = false,
                ServerStreamKind::Stderr => stderr_open = false,
            },
            Err(_) => break,
        }
    }

    join_server_thread(&mut stdout_thread);
    join_server_thread(&mut stderr_thread);

    match child.wait() {
        Ok(status) => status
            .code()
            .map(|code| code.clamp(0, super::MAX_EXIT_CODE))
            .unwrap_or(1),
        Err(error) => {
            write_server_fallback_error(
                stderr,
                format!("failed to wait for fallback {upstream_program} process: {error}"),
            );
            1
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ServerStreamKind {
    Stdout,
    Stderr,
}

enum ServerStreamMessage {
    Data(ServerStreamKind, Vec<u8>),
    Error(ServerStreamKind, io::Error),
    Finished(ServerStreamKind),
}

fn spawn_server_reader<R>(
    mut reader: R,
    kind: ServerStreamKind,
    sender: mpsc::Sender<ServerStreamMessage>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = vec![0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(ServerStreamMessage::Finished(kind));
                    break;
                }
                Ok(n) => {
                    if sender
                        .send(ServerStreamMessage::Data(kind, buffer[..n].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = sender.send(ServerStreamMessage::Error(kind, error));
                    break;
                }
            }
        }
    })
}

fn join_server_thread(handle: &mut Option<thread::JoinHandle<()>>) {
    if let Some(join) = handle.take() {
        let _ = join.join();
    }
}

fn terminate_server_process(
    child: &mut Child,
    stdout_thread: &mut Option<thread::JoinHandle<()>>,
    stderr_thread: &mut Option<thread::JoinHandle<()>>,
) {
    let _ = child.kill();
    let _ = child.wait();
    join_server_thread(stdout_thread);
    join_server_thread(stderr_thread);
}

fn write_server_fallback_error<Err: Write>(stderr: &mut Err, text: impl fmt::Display) {
    let mut sink = MessageSink::new(stderr);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Client);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{}", text);
    }
}
