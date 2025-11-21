#![deny(unsafe_code)]

use std::env;
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

/// Environment variable that selects the server implementation.
///
/// - "fallback" (default): use upstream rsync via the existing fallback mechanism.
/// - "native"           : parse --server arguments natively and then (currently)
///                        delegate to the fallback. This gives you a single,
///                        well-defined hook to wire the Rust engine into later.
const SERVER_IMPL_ENV: &str = "OC_RSYNC_SERVER_IMPL";

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

/// Top-level server-mode entry point used by the CLI front-end.
///
/// The implementation is selected by `OC_RSYNC_SERVER_IMPL`:
/// - "native"   → parse `--server` argv into `ServerInvocation`, then delegate
///                to the fallback (for now).
/// - anything else (or unset) → use the historical upstream rsync fallback.
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

    let impl_choice = env::var(SERVER_IMPL_ENV).unwrap_or_else(|_| String::from("fallback"));

    match impl_choice.as_str() {
        "native" => run_server_mode_native(args, stdout, stderr),
        _ => run_server_mode_fallback(args, stdout, stderr),
    }
}

/// Native server-mode entry point: parses the `--server` command-line into a
/// structured `ServerInvocation` and then delegates to the existing fallback.
///
/// This is intentionally conservative: it keeps behaviour identical to the
/// historical implementation while giving you a single hook (`invocation`)
/// to wire into a Rust-native server engine when you're ready.
fn run_server_mode_native<Out, Err>(
    args: &[OsString],
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let program_brand =
        super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    // Parse the server invocation. If parsing fails, emit a branded server
    // error and fall back to the historical implementation.
    let invocation = match ServerInvocation::parse(args) {
        Ok(invocation) => invocation,
        Err(text) => {
            write_server_error_message(stderr, program_brand, &text);
            // Preserve current behaviour by attempting the fallback anyway.
            return run_server_mode_fallback(args, stdout, stderr);
        }
    };

    // At this point we have a structured server invocation:
    //   - invocation.role
    //   - invocation.raw_flag_string (e.g. "-logDtpre.iLsfxC")
    //   - invocation.args (path arguments)
    //
    // When you're ready to implement a full Rust-native server, this is the
    // single place to call into your engine/protocol crate, e.g.:
    //
    //   return run_server_session(invocation, stdout, stderr);
    //
    // For now we preserve the historical behaviour by delegating to the
    // upstream rsync fallback, and we do NOT change the on-the-wire behaviour.
    let _ = invocation; // keep `invocation` used until you wire it in.
    run_server_mode_fallback(args, stdout, stderr)
}

/// Historical server implementation: delegate to an upstream `rsync` binary.
///
/// This is your previous `run_server_mode` logic, extracted into a helper so
/// that both the top-level entry point and the native parser can call it.
fn run_server_mode_fallback<Out, Err>(
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

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();
    let upstream_program = Brand::Upstream.client_program_name();
    let upstream_program_os = OsStr::new(upstream_program);
    let fallback = match fallback_override(CLIENT_FALLBACK_ENV) {
        Some(FallbackOverride::Disabled) => {
            let text = format!(
                "remote server mode is unavailable because OC_RSYNC_FALLBACK is disabled; set OC_RSYNC_FALLBACK to point to an upstream {upstream_program} binary"
            );
            write_server_fallback_error(stderr, program_brand, text);
            return 1;
        }
        Some(other) => other
            .resolve_or_default(upstream_program_os)
            .unwrap_or_else(|| OsString::from(upstream_program)),
        None => OsString::from(upstream_program),
    };

    let Some(resolved_fallback) = fallback_binary_path(fallback.as_os_str()) else {
        let diagnostic =
            describe_missing_fallback_binary(fallback.as_os_str(), &[CLIENT_FALLBACK_ENV]);
        write_server_fallback_error(stderr, program_brand, diagnostic);
        return 1;
    };

    if fallback_binary_is_self(&resolved_fallback) {
        let text = format!(
            "remote server mode is unavailable because the fallback binary '{}' resolves to this oc-rsync executable; install upstream {upstream_program} or set {CLIENT_FALLBACK_ENV} to a different path",
            resolved_fallback.display()
        );
        write_server_fallback_error(stderr, program_brand, text);
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
            write_server_fallback_error(stderr, program_brand, text);
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
                        program_brand,
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
                        program_brand,
                        format!("failed to forward fallback stderr: {error}"),
                    );
                    return 1;
                }
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stdout, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_fallback_error(
                    stderr,
                    program_brand,
                    format!("failed to read stdout from fallback {upstream_program}: {error}"),
                );
                return 1;
            }
            Ok(ServerStreamMessage::Error(ServerStreamKind::Stderr, error)) => {
                terminate_server_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                write_server_fallback_error(
                    stderr,
                    program_brand,
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
                program_brand,
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

fn write_server_fallback_error<Err: Write>(
    stderr: &mut Err,
    brand: Brand,
    text: impl fmt::Display,
) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Server);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
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

/// Native representation of a `--server` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerRole {
    Sender,
    Receiver,
}

#[derive(Debug, Clone)]
pub(crate) struct ServerInvocation {
    pub(crate) role: ServerRole,
    pub(crate) raw_flag_string: OsString,
    pub(crate) args: Vec<OsString>,
}

impl ServerInvocation {
    /// Parse an argv vector that contains `--server` (and optional `--sender`)
    /// in the upstream rsync wire protocol style.
    ///
    /// Expected shape (after the program name):
    ///
    ///   --server [--sender] -logDtpre.iLsfxC . /path
    ///
    pub(crate) fn parse(args: &[OsString]) -> Result<Self, String> {
        if args.is_empty() {
            return Err("missing program name for server invocation".to_string());
        }

        let mut iter = args.iter().skip(1);

        // Required: --server
        let Some(first) = iter.next() else {
            return Err("missing --server flag".to_string());
        };

        if first != "--server" {
            return Err(format!("expected --server, found {:?}", first));
        }

        // Optional: --sender
        let mut role = ServerRole::Receiver;
        let mut maybe_next = iter.next();

        if let Some(flag) = &maybe_next {
            if flag == "--sender" {
                role = ServerRole::Sender;
                maybe_next = iter.next();
            }
        }

        // Required: compact flagstring like -logDtpre.iLsfxC
        let flag_arg = match maybe_next {
            Some(v) if is_compact_flag_string(v) => v.clone(),
            Some(v) => {
                return Err(format!(
                    "expected compact server flag string (e.g. -logDtpre.iLsfxC), got {:?}",
                    v
                ));
            }
            None => {
                return Err("missing compact server flag string after --server".to_string());
            }
        };

        // Remaining args are the path arguments (e.g. "." and "/home/ofer/rsync").
        let remaining: Vec<OsString> = iter.cloned().collect();

        if remaining.is_empty() {
            return Err("server invocation requires at least one path argument".to_string());
        }

        Ok(ServerInvocation {
            role,
            raw_flag_string: flag_arg,
            args: remaining,
        })
    }
}

/// Heuristic check for a compact server flag string.
///
/// Upstream sends a single dash followed by a sequence of letters, digits,
/// and punctuation like '.' or ',' encoding option bits. We do not interpret
/// the flags here, just validate the overall shape.
fn is_compact_flag_string(value: &OsStr) -> bool {
    let s = value.to_string_lossy();
    let bytes = s.as_bytes();

    if bytes.len() < 2 || bytes[0] != b'-' {
        return false;
    }

    bytes[1..].iter().all(|b| {
        matches!(
            *b,
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b',' | b'_' | b'+'
        )
    })
}

/// Emit a branded server error message used by the native server path.
fn write_server_error_message<Err: Write>(stderr: &mut Err, brand: Brand, text: &str) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Server);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

