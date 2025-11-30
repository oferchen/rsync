#![deny(unsafe_code)]
//! Server mode entry points and argument parsing for `--server` invocations.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

use core::branding::Brand;
use core::fallback::{
    CLIENT_FALLBACK_ENV, FallbackOverride, describe_missing_fallback_binary, fallback_binary_path,
    fallback_override,
};
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
#[cfg(unix)]
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

/// Daemon mode is not supported on Windows.
#[cfg(not(unix))]
pub(crate) fn daemon_mode_arguments(_args: &[OsString]) -> Option<Vec<OsString>> {
    None
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
    let text = "daemon mode is not available on this platform";
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Daemon);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

/// Entry point for `--server` mode.
///
/// This is the public facade that:
/// - Flushes stdio (mirroring upstream).
/// - Consults the client-side fallback hook (CLIENT_FALLBACK_ENV).
/// - Detects the brand.
/// - Delegates to the embedded server dispatcher when no fallback is active.
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
    let fallback_override = fallback_override(CLIENT_FALLBACK_ENV);

    // Check if we should delegate to fallback
    let should_fallback = matches!(
        fallback_override,
        Some(FallbackOverride::Default | FallbackOverride::Explicit(_))
    );

    let invocation = match ServerInvocation::parse(args) {
        Ok(invocation) => invocation,
        Err(text) => {
            // If fallback is configured, delegate to it even if we can't parse the args
            if should_fallback {
                return run_server_fallback(
                    args,
                    program_brand,
                    stdout,
                    stderr,
                    fallback_override.unwrap(),
                );
            }
            write_server_error_message(stderr, program_brand, &text);
            return 1;
        }
    };

    let config = match invocation.into_server_config() {
        Ok(config) => config,
        Err(text) => {
            // If fallback is configured, delegate to it
            if should_fallback {
                return run_server_fallback(
                    args,
                    program_brand,
                    stdout,
                    stderr,
                    fallback_override.unwrap(),
                );
            }
            write_server_error_message(stderr, program_brand, &text);
            return 1;
        }
    };

    match core::server::run_server_stdio(config, &mut io::stdin(), &mut io::stdout()) {
        Ok(code) => code,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => match fallback_override {
            Some(FallbackOverride::Disabled) => {
                let text = format!(
                    "native server mode is unavailable and fallback delegation is disabled; \
                     set {CLIENT_FALLBACK_ENV} to point to an upstream rsync binary"
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

/// Local representation of the server-side role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvocationRole {
    Receiver,
    Generator,
}

impl InvocationRole {
    #[cfg(test)]
    pub(crate) fn as_server_role(self) -> ServerRole {
        match self {
            Self::Receiver => ServerRole::Receiver,
            Self::Generator => ServerRole::Generator,
        }
    }
}

/// Parsed `--server` invocation ready for dispatch.
#[derive(Debug)]
pub(crate) struct ServerInvocation {
    pub(crate) role: InvocationRole,
    pub(crate) raw_flag_string: String,
    pub(crate) args: Vec<OsString>,
}

impl ServerInvocation {
    /// Parse the argv vector of a `--server` invocation as generated by the client side.
    ///
    /// Expected shapes (mirroring upstream):
    /// - `rsync --server <flags> . <args...>`
    /// - `rsync --server --sender <flags> <args...>`
    ///
    /// For receiver mode we additionally support the common split-flag case:
    /// - `rsync --server -l ogDtpre.iLsfxC . <args...>`
    ///   which is normalised to `-logDtpre.iLsfxC`.
    pub(crate) fn parse(args: &[OsString]) -> Result<Self, String> {
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

        // Remaining args (which must not be empty) are passed through unchanged.
        let remaining_args: Vec<OsString> = args[index..].to_vec();

        if remaining_args.is_empty() {
            return Err("missing server arguments".to_string());
        }

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
    pub(crate) fn parse_flag_block(
        args: &[OsString],
        start: usize,
    ) -> Result<(String, usize), String> {
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
    pub(crate) fn into_server_config(self) -> Result<ServerConfig, String> {
        ServerConfig::from_flag_string_and_args(
            self.role.as_server_role(),
            self.raw_flag_string,
            self.args,
        )
    }

    /// Non-test version that uses core types directly.
    #[cfg(not(test))]
    fn into_server_config(self) -> Result<core::server::ServerConfig, String> {
        let role = match self.role {
            InvocationRole::Receiver => core::server::ServerRole::Receiver,
            InvocationRole::Generator => core::server::ServerRole::Generator,
        };
        core::server::ServerConfig::from_flag_string_and_args(role, self.raw_flag_string, self.args)
    }
}

/// Validate that the server flag string looks like an rsync `--server`
/// short-option block (e.g. `-logDtpre.iLsfxC`).
///
/// This accepts:
/// - Leading `-`.
/// - Alphanumeric characters.
/// - `.`, `,`, `_`, `+`.
///   while still rejecting obvious path-like arguments (which contain `/`).
pub(crate) fn is_rsync_flag_string(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'-' {
        return false;
    }

    if s.contains('/') {
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
///   while still rejecting obvious path-like arguments (which contain `/`).
pub(crate) fn is_rsync_flag_tail(s: &str) -> bool {
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

fn write_server_error_message<Err: Write>(stderr: &mut Err, brand: Brand, text: impl fmt::Display) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(
        1,
        "native server mode is not yet implemented; oc-rsync no longer delegates to upstream rsync binaries"
    );
    message = message.with_role(Role::Server);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(
            sink.writer_mut(),
            "native server mode is not yet implemented; oc-rsync no longer delegates to upstream rsync binaries",
        );
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
    use std::process::Command;

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

    // Upstream rsync flushes stdio before switching roles; we mirror that.
    let _ = stdout.flush();
    let _ = stderr.flush();

    // Build the fallback command preserving the original argv shape.
    let mut command = Command::new(&resolved_fallback);
    if args.len() > 1 {
        command.args(&args[1..]);
    }

    match command.status() {
        Ok(status) => status
            .code()
            .map(|c| c.clamp(0, super::MAX_EXIT_CODE))
            .unwrap_or(1),
        Err(error) => {
            let text = format!(
                "failed to execute fallback server '{}': {error}",
                resolved_fallback.display()
            );
            write_server_error_message(stderr, brand, text);
            1
        }
    }
}

/// Touch all fields of the `ServerInvocation` so that additions remain
/// Clippy-clean even if unused in some builds.
pub(crate) fn touch_server_invocation(invocation: &ServerInvocation) {
    let _role = invocation.role;
    let _flags = &invocation.raw_flag_string;
    let _args = &invocation.args;
}

#[cfg(test)]
mod tests {
    use super::{InvocationRole, ServerInvocation, is_rsync_flag_string, is_rsync_flag_tail};
    use std::ffi::OsString;

    #[test]
    fn parses_receiver_invocation() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-logDtpre.iLsfxC"),
            OsString::from("."),
            OsString::from("."),
        ];
        let invocation = ServerInvocation::parse(&args).expect("valid receiver args");
        assert_eq!(invocation.role, InvocationRole::Receiver);
        assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
        assert_eq!(invocation.args, vec![OsString::from(".")]);
    }

    #[test]
    fn parses_generator_invocation_with_sender_flag() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("-logDtpre.iLsfxC"),
            OsString::from("/tmp"),
        ];
        let invocation = ServerInvocation::parse(&args).expect("valid sender args");
        assert_eq!(invocation.role, InvocationRole::Generator);
        assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
        assert_eq!(invocation.args, vec![OsString::from("/tmp")]);
    }

    #[test]
    fn parses_split_flag_form() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-l"),
            OsString::from("ogDtpre.iLsfxC"),
            OsString::from("."),
            OsString::from("dest"),
        ];
        let invocation = ServerInvocation::parse(&args).expect("valid split flag args");
        assert_eq!(invocation.role, InvocationRole::Receiver);
        assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
        assert_eq!(invocation.args, vec![OsString::from("dest")]);
    }

    #[test]
    fn rejects_missing_flag_string() {
        let args = [OsString::from("rsync"), OsString::from("--server")];
        let error = ServerInvocation::parse(&args).unwrap_err();
        assert_eq!(error, "missing rsync flag string");
    }

    #[test]
    fn rejects_missing_server_marker() {
        let args = [OsString::from("rsync"), OsString::from("--sender")];
        let error = ServerInvocation::parse(&args).unwrap_err();
        assert!(error.contains("expected --server"));
    }

    #[test]
    fn rejects_missing_arguments() {
        let args = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-logDtpre.iLsfxC"),
            OsString::from("."),
        ];
        let error = ServerInvocation::parse(&args).unwrap_err();
        assert_eq!(error, "missing server arguments");
    }

    #[test]
    fn flag_string_validation() {
        assert!(is_rsync_flag_string("-logDtpre.iLsfxC"));
        assert!(is_rsync_flag_string("-av"));
        assert!(is_rsync_flag_string("-e.iLsfxC"));
        assert!(!is_rsync_flag_string("logDtpre")); // missing leading -
        assert!(!is_rsync_flag_string("-")); // too short
        assert!(!is_rsync_flag_string("-/path")); // contains /
    }

    #[test]
    fn flag_tail_validation() {
        assert!(is_rsync_flag_tail("ogDtpre.iLsfxC"));
        assert!(is_rsync_flag_tail("av"));
        assert!(!is_rsync_flag_tail("")); // empty
        assert!(!is_rsync_flag_tail("/path")); // contains /
    }

    #[test]
    fn server_mode_requested_detection() {
        use super::server_mode_requested;

        let with_server = [
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-av"),
        ];
        assert!(server_mode_requested(&with_server));

        let without_server = [
            OsString::from("rsync"),
            OsString::from("-av"),
            OsString::from("src"),
            OsString::from("dst"),
        ];
        assert!(!server_mode_requested(&without_server));

        let server_after_double_dash = [
            OsString::from("rsync"),
            OsString::from("--"),
            OsString::from("--server"),
        ];
        assert!(!server_mode_requested(&server_after_double_dash));
    }
}
