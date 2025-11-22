#![deny(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;

use core::branding::Brand;
use core::message::Role;
use core::rsync_error;
#[cfg(test)]
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

/// Return `true` if `--server` appears in the argument vector (before `--`).
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

/// Entry point for `--server` mode.
///
/// This is the public façade that:
/// - Flushes stdio (mirroring upstream).
/// - Detects the brand.
/// - Delegates to the embedded server dispatcher.
pub(crate) fn run_server_mode<Out, Err>(
    args: &[OsString],
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    // Upstream rsync flushes stdio before switching roles; we mirror that.
    let _ = stdout.flush();
    let _ = stderr.flush();

    let brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();
    run_server_mode_embedded(args, stderr, brand)
}

/// Strategy interface for executing a parsed server invocation.
///
/// This separates parsing from execution and allows battle-tested,
/// role-specific logic to be plugged in later without changing the call site.
trait ServerExecutor {
    fn execute(&self, invocation: &ServerInvocation, stderr: &mut dyn Write, brand: Brand) -> i32;
}

struct ReceiverExecutor;
struct GeneratorExecutor;

impl ServerExecutor for ReceiverExecutor {
    fn execute(&self, invocation: &ServerInvocation, stderr: &mut dyn Write, brand: Brand) -> i32 {
        // Keep invocation in scope so that future implementations can use it
        // without changing the interface.
        let _ = invocation;
        write_server_error_message(
            stderr,
            brand,
            "server mode is not yet implemented for role Receiver",
        );
        1
    }
}

impl ServerExecutor for GeneratorExecutor {
    fn execute(&self, invocation: &ServerInvocation, stderr: &mut dyn Write, brand: Brand) -> i32 {
        let _ = invocation;
        write_server_error_message(
            stderr,
            brand,
            "server mode is not yet implemented for role Generator",
        );
        1
    }
}

/// Factory for role-specific server executors.
///
/// This centralises the mapping between protocol role and concrete
/// implementation, so adding new roles or shims is a local change.
fn make_server_executor(role: InvocationRole) -> Box<dyn ServerExecutor> {
    match role {
        InvocationRole::Receiver => Box::new(ReceiverExecutor),
        InvocationRole::Generator => Box::new(GeneratorExecutor),
    }
}

/// Minimal embedded server-mode handler.
///
/// For now this only:
/// - Parses the `--server` invocation into a structured `ServerInvocation`.
/// - Resolves a role-specific executor.
/// - Emits a branded "not yet implemented" error.
///
/// The Strategy + Factory design here is intentionally stable so that the
/// real server engine can be wired in later without touching the CLI
/// entrypoints.
fn run_server_mode_embedded<Err: Write>(args: &[OsString], stderr: &mut Err, brand: Brand) -> i32 {
    let invocation = match ServerInvocation::parse(args) {
        Ok(invocation) => invocation,
        Err(error) => {
            write_server_error_message(stderr, brand, &error);
            return 1;
        }
    };

    let executor = make_server_executor(invocation.role);
    executor.execute(&invocation, stderr, brand)
}

/// Local representation of a parsed `--server` invocation.
///
/// This acts as a stable value object between the argv world and
/// the internal server engine, mirroring the way upstream rsync
/// distinguishes between receiver and generator roles.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum InvocationRole {
    Receiver,
    Generator,
}

impl InvocationRole {
    #[cfg(test)]
    fn as_server_role(self) -> ServerRole {
        match self {
            Self::Receiver => ServerRole::Receiver,
            Self::Generator => ServerRole::Generator,
        }
    }
}

#[derive(Debug)]
struct ServerInvocation {
    role: InvocationRole,
    raw_flag_string: String,
    args: Vec<OsString>,
}

impl ServerInvocation {
    /// Parse the argv vector of a `--server` invocation as generated by the
    /// client side.
    ///
    /// Expected shapes (mirroring upstream):
    /// - `rsync --server <flags> . <args...>`
    /// - `rsync --server --sender <flags> <args...>`
    fn parse(args: &[OsString]) -> Result<Self, String> {
        let first = args
            .first()
            .ok_or_else(|| "missing program name".to_string())?;
        if first.is_empty() {
            return Err("missing program name".to_string());
        }

        let second = args
            .get(1)
            .ok_or_else(|| "missing rsync server marker".to_string())?;
        if second != "--server" {
            return Err(format!("expected --server, found {second:?}"));
        }

        let mut index = 2usize;
        let mut role = InvocationRole::Receiver;

        if let Some(flag) = args.get(index) {
            if flag == "--sender" {
                role = InvocationRole::Generator;
                index += 1;
            }
        }

        let (flag_string, mut index) = Self::parse_flag_block(args, index)?;

        // For receiver mode, a "." placeholder is accepted and normalised away,
        // but it is not strictly required. This keeps us tolerant of slightly
        // different remote-shell behaviours.
        if role == InvocationRole::Receiver {
            if let Some(component) = args.get(index) {
                if component == "." {
                    index += 1;
                }
            }
        }

        // Remaining args (which may be empty) are passed through unchanged.
        let remaining_args: Vec<OsString> = args[index..].to_vec();

        let invocation = ServerInvocation {
            role,
            raw_flag_string: flag_string,
            args: remaining_args,
        };

        touch_server_invocation(&invocation);
        Ok(invocation)
    }

    /// Parse the flag block, accepting both contiguous and split forms:
    /// - `-logDtpre.iLsfxC`
    /// - `-l ogDtpre.iLsfxC`  (combined into `-logDtpre.iLsfxC`)
    fn parse_flag_block(args: &[OsString], start: usize) -> Result<(String, usize), String> {
        let head = args
            .get(start)
            .ok_or_else(|| "missing rsync flag string".to_string())?;

        // Take ownership up front to avoid `Cow` move pitfalls.
        let head_str: String = head.to_string_lossy().into_owned();

        // Prefer the combined head+tail form when we see a short "-X" head
        // followed by a valid tail fragment. This matches split forms such as:
        //   - `-l ogDtpre.iLsfxC`
        if let Some(split_tail) = args.get(start + 1) {
            let head_bytes = head_str.as_bytes();
            if head_bytes.len() == 2 && head_bytes[0] == b'-' {
                let tail_str: String = split_tail.to_string_lossy().into_owned();
                if is_rsync_flag_tail(&tail_str) {
                    let mut combined = head_str.clone();
                    combined.push_str(&tail_str);
                    if is_rsync_flag_string(&combined) {
                        return Ok((combined, start + 2));
                    }
                }
            }
        }

        // Fall back to treating the head as the complete flag string.
        if is_rsync_flag_string(&head_str) {
            return Ok((head_str, start + 1));
        }

        Err(format!("invalid rsync server flag string: {head_str:?}"))
    }

    /// Helper used by tests to convert a parsed invocation into the core
    /// `ServerConfig` structure.
    #[cfg(test)]
    fn into_server_config(self) -> Result<ServerConfig, String> {
        ServerConfig::from_flag_string_and_args(
            self.role.as_server_role(),
            self.raw_flag_string,
            self.args,
        )
    }
}

/// Validate that the server flag string looks like an rsync `--server`
/// short-option block (e.g. `-logDtpre.iLsfxC`).
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

/// Validate a tail fragment of a flag string (no leading '-').
///
/// This allows us to accept split forms like:
/// - `-l ogDtpre.iLsfxC`
///   while still rejecting obvious path-like arguments (which contain '/').
fn is_rsync_flag_tail(s: &str) -> bool {
    if s.is_empty() || s.contains('/') {
        return false;
    }

    s.bytes().all(|b| {
        matches!(
            b,
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b',' | b'_' | b'+'
        )
    })
}

fn write_server_error_message(stderr: &mut dyn Write, brand: Brand, text: &str) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Daemon);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

/// Touch all fields of the `ServerInvocation` so that additions remain
/// Clippy-clean even if unused in some builds.
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
    fn parses_receiver_invocation_with_split_flag_block() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-l"),
            OsString::from("ogDtpre.iLsfxC"),
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
