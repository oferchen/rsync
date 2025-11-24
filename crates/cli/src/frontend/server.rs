#![deny(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use core::branding::Brand;
use core::fallback::{
    CLIENT_FALLBACK_ENV, FallbackOverride, describe_missing_fallback_binary,
    fallback_binary_is_self, fallback_binary_path, fallback_override,
};
use core::message::Role;
use core::rsync_error;
use logging::MessageSink;

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

/// Delegates execution to the daemon front-end (Unix) or reports that daemon
/// mode is unavailable (Windows).
#[cfg(unix)]
pub(crate) fn run_daemon_mode<Out, Err>(
    args: Vec<OsString>,
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    // On Unix, delegate to the actual daemon front-end.
    daemon::run(args, stdout, stderr)
}

#[cfg(windows)]
pub(crate) fn run_daemon_mode<Out, Err>(
    args: Vec<OsString>,
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    write_daemon_unavailable_error(stderr, program_brand);
    1
}

/// Executes the native server entry point when `--server` is requested.
pub(crate) fn run_server_mode<Out, Err>(
    args: &[OsString],
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    run_server_mode_embedded(args, stdout, stderr)
}

fn run_server_mode_embedded<Out, Err>(args: &[OsString], stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();
    let invocation = match ServerInvocation::parse(args) {
        Ok(invocation) => invocation,
        Err(text) => {
            write_server_error_message(stderr, program_brand, &text);
            return 1;
        }
    };

    let config = match invocation.into_server_config() {
        Ok(config) => config,
        Err(text) => {
            write_server_error_message(stderr, program_brand, &text);
            return 1;
        }
    };

    let fallback_override = fallback_override(CLIENT_FALLBACK_ENV);

    match core::server::run_server_stdio(config, &mut std::io::stdin(), &mut std::io::stdout()) {
        Ok(code) => code,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => match fallback_override {
            Some(FallbackOverride::Disabled) => {
                let text = format!(
                    "native server mode is unavailable and fallback delegation is disabled; set {CLIENT_FALLBACK_ENV} to point to an upstream rsync binary"
                );
                write_server_error_message(stderr, program_brand, text);
                1
            }
            Some(choice) => run_server_fallback(args, program_brand, stdout, stderr, choice),
            None => {
                write_server_error_message(stderr, program_brand, error);
                1
            }
        },
        Err(error) => {
            let text = format!("server execution failed: {error}");
            write_server_error_message(stderr, program_brand, &text);
            1
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoleKind {
    Receiver,
    Generator,
}

#[derive(Debug)]
struct ServerInvocation {
    role: RoleKind,
    raw_flag_string: String,
    args: Vec<OsString>,
}

impl ServerInvocation {
    fn parse(args: &[OsString]) -> Result<Self, String> {
        if args.len() < 3 {
            return Err("invalid server invocation: missing arguments".to_string());
        }

        let mut iter = args.iter().skip(1);
        match iter.next() {
            Some(flag) if flag == "--server" => {}
            _ => return Err("invalid server invocation: expected --server".to_string()),
        }

        let mut role = RoleKind::Receiver;
        let mut maybe_flag_string: Option<&OsStr> = None;
        for candidate in iter.by_ref() {
            if maybe_flag_string.is_none() && candidate == "--sender" {
                role = RoleKind::Generator;
                continue;
            }

            maybe_flag_string = Some(candidate);
            break;
        }

        let Some(flag_string) = maybe_flag_string else {
            return Err("missing rsync server flag string".to_string());
        };

        let mut remaining: Vec<OsString> = iter.cloned().collect();
        if let Some(first) = remaining.first() {
            if first == "." {
                remaining.remove(0);
            }
        }

        Ok(Self {
            role,
            raw_flag_string: flag_string
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "flag string must be valid UTF-8".to_string())?,
            args: remaining,
        })
    }

    fn into_server_config(self) -> Result<core::server::config::ServerConfig, String> {
        let role = match self.role {
            RoleKind::Receiver => core::server::role::ServerRole::Receiver,
            RoleKind::Generator => core::server::role::ServerRole::Generator,
        };

        core::server::config::ServerConfig::from_flag_string_and_args(
            role,
            self.raw_flag_string,
            self.args,
        )
    }
}

fn write_server_error_message<Err: Write>(stderr: &mut Err, brand: Brand, text: impl fmt::Display) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Server);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

fn run_server_fallback<Out, Err>(
    args: &[OsString],
    brand: Brand,
    stdout: &mut Out,
    stderr: &mut Err,
    override_choice: FallbackOverride,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let upstream_program = Brand::Upstream.client_program_name();
    let upstream_program_os = OsStr::new(upstream_program);
    let fallback = override_choice
        .resolve_or_default(upstream_program_os)
        .unwrap_or_else(|| OsString::from(upstream_program));

    let Some(resolved_fallback) = fallback_binary_path(fallback.as_os_str()) else {
        let diagnostic =
            describe_missing_fallback_binary(fallback.as_os_str(), &[CLIENT_FALLBACK_ENV]);
        write_server_error_message(stderr, brand, diagnostic);
        return 1;
    };

    if fallback_binary_is_self(&resolved_fallback) {
        let text = format!(
            "remote server mode is unavailable because the fallback binary '{}' resolves to this oc-rsync executable; install upstream {upstream_program} or set {CLIENT_FALLBACK_ENV} to a different path",
            resolved_fallback.display()
        );
        write_server_error_message(stderr, brand, text);
        return 1;
    }

    let mut command = Command::new(&resolved_fallback);
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
            write_server_error_message(stderr, brand, text);
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
                    write_server_error_message(
                        stderr,
                        brand,
                        format!("failed to forward fallback stdout: {error}"),
                    );
                    return 1;
                }
            }
            Ok(ServerStreamMessage::Data(ServerStreamKind::Stderr, data)) => {
                if let Err(error) = stderr.write_all(&data) {
                    terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    write_server_error_message(
                        stderr,
                        brand,
                        format!("failed to forward fallback stderr: {error}"),
                    );
                    return 1;
                }
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stdout, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_error_message(
                    stderr,
                    brand,
                    format!("failed to read stdout from fallback {upstream_program}: {error}"),
                );
                return 1;
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stderr, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_error_message(
                    stderr,
                    brand,
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
            write_server_error_message(
                stderr,
                brand,
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

#[cfg(windows)]
fn write_daemon_unavailable_error<Err: Write>(stderr: &mut Err, brand: Brand) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(
        1,
        "daemon mode is not supported on this platform; run the oc-rsync daemon on a Unix-like system"
    );
    message = message.with_role(Role::Client);

    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(
            sink.writer_mut(),
            "daemon mode is not supported on this platform; run the oc-rsync daemon on a Unix-like system"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_missing_server_flag() {
        let args = [OsString::from("rsync"), OsString::from("--sender")];
        let error = ServerInvocation::parse(&args).unwrap_err();
        assert_eq!(error, "invalid server invocation: missing arguments");
    }

    #[test]
    fn parse_accepts_placeholder_dot() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("-logDtpre.iLsfxC"),
            OsString::from("."),
            OsString::from("/tmp"),
        ];

        let parsed = ServerInvocation::parse(&args).expect("parse invocation");
        assert_eq!(parsed.role, RoleKind::Generator);
        assert_eq!(parsed.raw_flag_string, "-logDtpre.iLsfxC");
        assert_eq!(parsed.args, vec![OsString::from("/tmp")]);
    }
}
