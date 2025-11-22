#![deny(unsafe_code)]

use std::env;
use std::ffi::OsString;
use std::io::Write;

use core::branding::Brand;
use core::message::Role;
use core::rsync_error;
use core::server::{ServerConfig, ServerRole};
use logging::MessageSink;

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
    // Flush buffers before invoking the server.
    let _ = stdout.flush();
    let _ = stderr.flush();

    // Ignore OC_RSYNC_SERVER_IMPL overrides and always use the embedded handler.
    run_server_mode_embedded(args, stdout, stderr)
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
    // Treat these diagnostics as daemon/server-side errors for logging purposes.
    message = message.with_role(Role::Daemon);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_receiver_invocation_and_normalises_dot_placeholder() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-logDtpre.iLsfxC"),
            OsString::from("."),
            OsString::from("dest"),
        ];

        let invocation = ServerInvocation::parse(&args).expect("invocation parses");
        assert_eq!(invocation.role, InvocationRole::Receiver);
        assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
        assert_eq!(invocation.args, vec![OsString::from("dest")]);

        let config = invocation.into_server_config().expect("config parses");
        assert_eq!(config.role, ServerRole::Receiver);
        assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
        assert_eq!(config.args, vec![OsString::from("dest")]);
    }

    #[test]
    fn parses_sender_invocation_without_placeholder() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("-logDtpre.iLsfxC"),
            OsString::from("relative"),
            OsString::from("dest"),
        ];

        let invocation = ServerInvocation::parse(&args).expect("invocation parses");
        assert_eq!(invocation.role, InvocationRole::Generator);
        assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
        assert_eq!(
            invocation.args,
            vec![OsString::from("relative"), OsString::from("dest")]
        );

        let config = invocation.into_server_config().expect("config parses");
        assert_eq!(config.role, ServerRole::Generator);
        assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
        assert_eq!(
            config.args,
            vec![OsString::from("relative"), OsString::from("dest")]
        );
    }

    #[test]
    fn parse_rejects_missing_flag_string() {
        let args = [OsString::from("rsync"), OsString::from("--server")];

        let error = ServerInvocation::parse(&args).expect_err("parse should fail");
        assert!(error.contains("flag string"));
    }
}
