#![deny(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;

use core::branding::Brand;
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

fn run_server_mode_embedded<Out, Err>(args: &[OsString], _stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = _stdout.flush();
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
