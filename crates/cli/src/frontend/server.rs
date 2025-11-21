#![deny(unsafe_code)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::process::{Command, Stdio};

use core::branding::Brand;
use core::fallback::{
    CLIENT_FALLBACK_ENV, describe_missing_fallback_binary, fallback_binary_is_self,
    fallback_binary_path,
};
use core::message::Role;
use core::rsync_error;
use logging::MessageSink;

const SERVER_IMPL_ENV: &str = "OC_RSYNC_SERVER_IMPL";

/// Translate a client invocation that requested `--daemon` into a standalone
/// daemon invocation.
///
/// This mirrors upstream rsync's `--daemon` handling: it replaces the program
/// name with the daemon binary name and forwards the remaining arguments,
/// preserving `--` handling.
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

    for arg in &args[1..] {
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

    if !found {
        return None;
    }

    Some(daemon_args)
}

/// Returns true if the command-line arguments indicate server mode.
///
/// This looks for the `--server` flag in the initial argument vector, before
/// any `--` separator.
pub(crate) fn server_mode_requested(args: &[OsString]) -> bool {
    let mut reached_double_dash = false;

    for arg in args.iter().skip(1) {
        if !reached_double_dash && arg == "--" {
            reached_double_dash = true;
            continue;
        }

        if !reached_double_dash && arg == "--server" {
            return true;
        }
    }

    false
}

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
    // On Unix we have a real daemon implementation: delegate to the `daemon`
    // crate, which already mirrors upstream behaviour.
    daemon::run(args, stdout, stderr)
}

#[cfg(windows)]
pub(crate) fn run_daemon_mode<Out, Err>(
    _args: Vec<OsString>,
    _stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let brand = super::detect_program_name(None).brand();
    write_daemon_unavailable_error(stderr, brand);
    1
}

#[cfg(windows)]
fn write_daemon_unavailable_error<Err: Write>(stderr: &mut Err, brand: Brand) {
    let text = "daemon mode is not available on this platform".to_string();
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Daemon);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

/// Selects the implementation used for server mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerBackend {
    /// Use the embedded Rust implementation.
    Embedded,
    /// Delegate to an external binary discovered via the fallback mechanism.
    Fallback,
    /// Delegate to the upstream `rsync` binary explicitly.
    Upstream,
}

/// Parse `OC_RSYNC_SERVER_IMPL` into a `ServerBackend`.
fn detect_server_backend_from_env() -> Option<ServerBackend> {
    match env::var(SERVER_IMPL_ENV) {
        Ok(value) => match value.as_str() {
            "embedded" => Some(ServerBackend::Embedded),
            "fallback" => Some(ServerBackend::Fallback),
            "upstream" => Some(ServerBackend::Upstream),
            _ => None,
        },
        Err(_) => None,
    }
}

/// Dispatch server mode to the selected backend.  Defaults to the upstream binary.
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

    let backend = detect_server_backend_from_env().unwrap_or(ServerBackend::Upstream);

    match backend {
        ServerBackend::Embedded => run_server_mode_embedded(args, stdout, stderr),
        ServerBackend::Fallback => run_server_mode_fallback(args, stdout, stderr),
        ServerBackend::Upstream => run_server_mode_upstream(args, stdout, stderr),
    }
}

/// Run server mode using the embedded implementation.
///
/// For now this only validates the invocation and reports an rsync-style error,
/// mirroring upstream's behaviour for invalid `--server` argument vectors.
fn run_server_mode_embedded<Out, Err>(args: &[OsString], _stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = _stdout.flush();
    let _ = stderr.flush();

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();
    let invocation = match ServerInvocation::parse(args) {
        Ok(invocation) => {
            // Keep the parsed structure alive to discourage bitrot in the parser.
            touch_server_invocation(&invocation);
            invocation
        }
        Err(text) => {
            write_server_error_message(stderr, program_brand, &text);
            return 1;
        }
    };

    // For now we do not yet implement the actual data transfer; upstream rsync
    // reports usage errors for invalid invocations, which is what the embedding
    // tests rely on. A valid invocation would reach this point.
    let text = format!(
        "server mode is not yet implemented for role {:?}",
        invocation.role
    );
    write_server_error_message(stderr, program_brand, &text);
    1
}

/// Run server mode by delegating to the external fallback binary.
///
/// This uses the same binary-discovery mechanism as the client front-end. The
/// `CLIENT_FALLBACK_ENV` environment variable is only used for diagnostics.
fn run_server_mode_fallback<Out, Err>(args: &[OsString], stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();
    let upstream_program = Brand::Upstream.client_program_name();
    let upstream_program_os = OsStr::new(upstream_program);

    let fallback = match fallback_binary_path(upstream_program_os) {
        Some(path) => path,
        None => {
            let text =
                describe_missing_fallback_binary(upstream_program_os, &[CLIENT_FALLBACK_ENV]);
            write_server_error_message(stderr, program_brand, &text);
            return 1;
        }
    };

    if fallback_binary_is_self(&fallback) {
        let text =
            "remote server mode fallback binary resolved to this process, refusing to recurse";
        write_server_error_message(stderr, program_brand, text);
        return 1;
    }

    match spawn_fallback_server(fallback.as_os_str(), args) {
        Ok(status) => status,
        Err(err) => {
            let text = format!("failed to execute fallback server: {err}");
            write_server_error_message(stderr, program_brand, &text);
            1
        }
    }
}

/// Invoke the upstream `rsync` binary in server mode.
///
/// This version does not rely on any fallback helper; it directly spawns
/// `Brand::Upstream.client_program_name()` and forwards all arguments after
/// `--server`, exactly as upstream `rsync` would do.
fn run_server_mode_upstream<Out, Err>(
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

    match spawn_upstream_server(OsStr::new(upstream_program), args) {
        Ok(status) => status,
        Err(err) => {
            let text = format!("failed to execute upstream server: {err}");
            write_server_error_message(stderr, program_brand, &text);
            1
        }
    }
}

/// Spawn the upstream server process and forward all arguments after `--server`.
fn spawn_upstream_server(program: &OsStr, args: &[OsString]) -> io::Result<i32> {
    let mut command = Command::new(program);

    // Forward everything from `--server` onwards, as upstream does.
    let mut saw_server = false;
    for arg in args.iter().skip(1) {
        if !saw_server && arg == "--server" {
            saw_server = true;
        }
        if saw_server {
            command.arg(arg);
        }
    }

    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    Ok(status.code().unwrap_or(1))
}

/// Spawn a fallback or upstream server process and wire it up to this
/// process's standard streams.
///
/// This mirrors upstream rsync's behaviour for remote server delegation: the
/// child inherits stdin/stdout/stderr and we simply wait for its exit status.
fn spawn_fallback_server(program: &OsStr, args: &[OsString]) -> io::Result<i32> {
    let mut command = Command::new(program);

    // Upstream forwards everything from `--server` onwards for the remote
    // side, so we do the same.
    let mut saw_server = false;
    for arg in args.iter().skip(1) {
        if !saw_server && arg == "--server" {
            saw_server = true;
        }
        if saw_server {
            command.arg(arg);
        }
    }

    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    Ok(status.code().unwrap_or(1))
}

/// Parsed representation of a `--server` invocation.
#[derive(Debug)]
struct ServerInvocation {
    role: Role,
    raw_flag_string: String,
    args: Vec<OsString>,
}

impl ServerInvocation {
    fn parse(args: &[OsString]) -> Result<Self, String> {
        if args.is_empty() {
            return Err("missing program name".to_string());
        }

        // Upstream expects:
        //   rsync --server <flags> . <args...>
        //
        // We keep this strict to make it easier to reason about.
        let mut iter = args.iter();
        let _program = iter.next().unwrap();

        let first = match iter.next() {
            Some(s) => s.to_string_lossy().into_owned(),
            None => return Err("missing --server flag".to_string()),
        };

        if first != "--server" {
            return Err(format!("expected --server, found {first:?}"));
        }

        let mut role = Role::Receiver;
        let mut maybe_next = iter.next();

        if let Some(flag) = maybe_next {
            if flag == "--sender" {
                role = Role::Generator;
                maybe_next = iter.next();
            }
        }

        let flag_string = match maybe_next {
            Some(s) => s.to_string_lossy().into_owned(),
            None => return Err("missing rsync flag string".to_string()),
        };

        if !is_rsync_flag_string(&flag_string) {
            return Err(format!("invalid rsync server flag string: {flag_string:?}"));
        }

        // Upstream uses "." as the next argument; we validate but otherwise
        // ignore it here.
        let dot = match iter.next() {
            Some(s) => s,
            None => return Err("missing server path component".to_string()),
        };

        if dot != "." {
            return Err(format!(
                "expected server path placeholder '.', found {dot:?}"
            ));
        }

        let remaining_args: Vec<OsString> = iter.cloned().collect();

        if remaining_args.is_empty() {
            return Err("missing server arguments".to_string());
        }

        let invocation = ServerInvocation {
            role,
            raw_flag_string: flag_string,
            args: remaining_args,
        };

        // Keep this local helper updated when we add fields.
        touch_server_invocation(&invocation);

        Ok(invocation)
    }
}

/// Returns true if `s` looks like an rsync server flag string.
///
/// Upstream uses a compact flag-string format consisting of a leading `-`
/// followed by alphanumeric and a small set of punctuation characters.
fn is_rsync_flag_string(s: &str) -> bool {
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

fn write_server_error_message<Err: Write>(stderr: &mut Err, brand: Brand, text: &str) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Server);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

/// Dummy use of all fields in `ServerInvocation` to prevent it from becoming
/// partially-dead when we extend it.
///
/// This helps keep the parser and the usages in sync.
fn touch_server_invocation(invocation: &ServerInvocation) {
    let _role = invocation.role;
    let _flags = &invocation.raw_flag_string;
    let _args = &invocation.args;
}
