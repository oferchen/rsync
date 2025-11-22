#![deny(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;

use core::branding::Brand;
use core::message::Role;
use core::rsync_error;
use logging::MessageSink;

use core::server::{ServerConfig, ServerRole};

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

/// Runs the embedded server implementation when `--server` is requested.
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

    match core::server::run_server_stdio(config, &mut std::io::stdin(), &mut std::io::stdout()) {
        Ok(code) => code,
        Err(error) => {
            let text = format!("server I/O error: {error}");
            write_server_error_message(stderr, program_brand, &text);
            1
        }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvocationRole {
    Receiver,
    Generator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ServerInvocation {
    role: InvocationRole,
    raw_flag_string: String,
    args: Vec<OsString>,
}

impl ServerInvocation {
    fn parse(args: &[OsString]) -> Result<Self, String> {
        let mut iter = args.iter();
        let Some(program) = iter.next() else {
            return Err("missing program name".to_string());
        };

        let Some(flag) = iter.next() else {
            let program = program.to_string_lossy();
            return Err(format!("{program}: --server must be supplied"));
        };

        if flag != "--server" {
            let program = program.to_string_lossy();
            return Err(format!(
                "{program}: unexpected argument {:?}; expected --server",
                flag
            ));
        }

        let mut role = InvocationRole::Receiver;
        let current = match iter.next() {
            Some(arg) if arg == "--sender" => {
                role = InvocationRole::Generator;
                iter.next()
            }
            other => other,
        };

        let Some(flag_string) = current else {
            return Err("missing rsync server flag string".to_string());
        };

        let mut remaining: Vec<OsString> = iter.cloned().collect();
        if remaining
            .first()
            .map_or(false, |arg| arg.as_os_str() == OsStr::new("."))
        {
            remaining.remove(0);
        }

        Ok(Self {
            role,
            raw_flag_string: flag_string.to_string_lossy().into_owned(),
            args: remaining,
        })
    }

    fn into_server_config(self) -> Result<ServerConfig, String> {
        let role = match self.role {
            InvocationRole::Receiver => ServerRole::Receiver,
            InvocationRole::Generator => ServerRole::Generator,
        };

        ServerConfig::from_flag_string_and_args(role, self.raw_flag_string, self.args)
    }
}

#[cfg(test)]
mod tests {
    use super::{InvocationRole, ServerInvocation};
    use std::ffi::OsString;

    #[test]
    fn parses_sender_invocation_with_placeholder() {
        let args = vec![
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("-logDtpre.iLsfxC"),
            OsString::from("."),
            OsString::from("path"),
        ];

        let parsed = ServerInvocation::parse(&args).expect("parse succeeds");
        assert_eq!(parsed.role, InvocationRole::Generator);
        assert_eq!(parsed.raw_flag_string, "-logDtpre.iLsfxC");
        assert_eq!(parsed.args, vec![OsString::from("path")]);
    }

    #[test]
    fn errors_when_missing_flag_string() {
        let args = vec![OsString::from("rsync"), OsString::from("--server")];
        let err = ServerInvocation::parse(&args).expect_err("parse fails");
        assert!(err.contains("flag string"));
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
