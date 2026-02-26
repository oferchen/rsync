#![deny(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

use core::branding::Brand;
use core::message::Role;
use core::rsync_error;
use logging_sink::MessageSink;

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
///
/// Only considers `--server` appearing before any `--` separator.
pub(crate) fn server_mode_requested(args: &[OsString]) -> bool {
    for arg in args.iter().skip(1) {
        if arg == "--" {
            return false;
        }
        if arg == "--server" {
            return true;
        }
    }
    false
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

/// Runs the native server implementation when `--server` is requested.
pub(crate) fn run_server_mode<Out, Err>(
    args: &[OsString],
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    use core::server::{ServerConfig, ServerRole, run_server_stdio};

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    // Parse role from --sender/--receiver flags
    let is_sender = args.iter().any(|a| a == "--sender");
    let is_receiver = args.iter().any(|a| a == "--receiver");
    let ignore_errors = args.iter().any(|a| a == "--ignore-errors");
    let fsync = args.iter().any(|a| a == "--fsync");
    let io_uring_policy = if args.iter().any(|a| a == "--io-uring") {
        fast_io::IoUringPolicy::Enabled
    } else if args.iter().any(|a| a == "--no-io-uring") {
        fast_io::IoUringPolicy::Disabled
    } else {
        fast_io::IoUringPolicy::Auto
    };

    // Detect secluded-args mode: `-s` flag appears as a standalone argument
    // after --server. upstream: options.c — protect_args in server mode.
    let secluded_args = detect_secluded_args_flag(args);

    let role = if is_sender {
        ServerRole::Generator // Server sends files to client (generator role)
    } else if is_receiver {
        ServerRole::Receiver // Server receives files from client
    } else {
        // Default to receiver if neither specified (upstream behavior)
        ServerRole::Receiver
    };

    let mut stdin = io::stdin().lock();

    // When secluded-args is active, read the full argument list from stdin
    // before parsing server flags. The client sends arguments as
    // null-separated strings terminated by an empty string.
    // upstream: main.c — read_args() reads protected args from stdin.
    let effective_args: Vec<OsString>;
    let (flag_string, positional_args) = if secluded_args {
        match protocol::secluded_args::recv_secluded_args(&mut stdin) {
            Ok(received_args) => {
                effective_args = received_args.into_iter().map(OsString::from).collect();
                parse_server_flag_string_and_args(&effective_args)
            }
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("failed to read secluded args: {e}"),
                );
                return 1;
            }
        }
    } else {
        parse_server_flag_string_and_args(&args[1..])
    };

    // Build server configuration
    let mut config =
        match ServerConfig::from_flag_string_and_args(role, flag_string, positional_args) {
            Ok(cfg) => cfg,
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid server arguments: {e}"),
                );
                return 1;
            }
        };

    // Apply additional flags parsed from full arguments
    config.ignore_errors = ignore_errors;
    config.fsync = fsync;
    config.io_uring_policy = io_uring_policy;

    // Run native server with stdio
    match run_server_stdio(config, &mut stdin, stdout) {
        Ok(_stats) => {
            // Success
            0
        }
        Err(e) => {
            write_server_error(stderr, program_brand, format!("server error: {e}"));
            1
        }
    }
}

/// Detects whether secluded-args mode is requested in the server arguments.
///
/// In secluded-args mode, the client sends `-s` as a standalone argument
/// on the command line (not as part of a combined flag string). The server
/// then reads the full argument list from stdin before proceeding.
fn detect_secluded_args_flag(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|a| a == "-s")
}

/// Parses the flag string and positional arguments from server-mode argument list.
///
/// This extracts the compact flag string (first arg starting with `-` that is not
/// a known long flag) and positional arguments (everything after the flag string
/// and optional `.` separator).
fn parse_server_flag_string_and_args(args: &[OsString]) -> (String, Vec<OsString>) {
    let mut flag_string = String::new();
    let mut positional_args = Vec::new();
    let mut found_flags = false;

    for arg in args {
        let arg_str = arg.to_string_lossy();

        // Skip known long-form arguments and secluded-args flag
        if arg_str == "--server"
            || arg_str == "--sender"
            || arg_str == "--receiver"
            || arg_str == "--ignore-errors"
            || arg_str == "--fsync"
            || arg_str == "--io-uring"
            || arg_str == "--no-io-uring"
            || arg_str == "-s"
        {
            continue;
        }

        // First arg starting with '-' is the flag string
        if !found_flags && arg_str.starts_with('-') {
            flag_string = arg_str.into_owned();
            found_flags = true;
            continue;
        }

        // Skip the "." separator if present (upstream uses this as a placeholder)
        if found_flags && arg_str == "." {
            continue;
        }

        // Everything else is a positional argument
        if found_flags {
            positional_args.push(arg.clone());
        }
    }

    (flag_string, positional_args)
}

fn write_server_error<Err: Write>(stderr: &mut Err, brand: Brand, text: impl fmt::Display) {
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

    // ==================== daemon_mode_arguments tests ====================

    #[test]
    fn daemon_mode_arguments_empty_args() {
        let args: Vec<OsString> = vec![];
        assert!(daemon_mode_arguments(&args).is_none());
    }

    #[test]
    fn daemon_mode_arguments_no_daemon_flag() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("-av"),
            OsString::from("src/"),
            OsString::from("dest/"),
        ];
        assert!(daemon_mode_arguments(&args).is_none());
    }

    #[test]
    fn daemon_mode_arguments_with_daemon_flag() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--daemon"),
            OsString::from("--port=8873"),
        ];
        let result = daemon_mode_arguments(&args);
        assert!(result.is_some());
        let daemon_args = result.unwrap();
        // Should have program name and --port=8873, but not --daemon
        assert!(daemon_args.iter().any(|a| a == "--port=8873"));
        assert!(!daemon_args.iter().any(|a| a == "--daemon"));
    }

    #[test]
    fn daemon_mode_arguments_daemon_flag_with_config() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--daemon"),
            OsString::from("--config=/etc/rsyncd.conf"),
            OsString::from("--no-detach"),
        ];
        let result = daemon_mode_arguments(&args);
        assert!(result.is_some());
        let daemon_args = result.unwrap();
        assert!(daemon_args.iter().any(|a| a == "--config=/etc/rsyncd.conf"));
        assert!(daemon_args.iter().any(|a| a == "--no-detach"));
    }

    #[test]
    fn daemon_mode_arguments_daemon_after_double_dash_ignored() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("-av"),
            OsString::from("--"),
            OsString::from("--daemon"),
        ];
        // --daemon after -- should not trigger daemon mode
        let result = daemon_mode_arguments(&args);
        assert!(result.is_none());
    }

    #[test]
    fn daemon_mode_arguments_double_dash_preserved() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--daemon"),
            OsString::from("--"),
            OsString::from("extra-arg"),
        ];
        let result = daemon_mode_arguments(&args);
        assert!(result.is_some());
        let daemon_args = result.unwrap();
        assert!(daemon_args.iter().any(|a| a == "--"));
        assert!(daemon_args.iter().any(|a| a == "extra-arg"));
    }

    #[test]
    fn daemon_mode_arguments_program_only() {
        let args: Vec<OsString> = vec![OsString::from("rsync")];
        assert!(daemon_mode_arguments(&args).is_none());
    }

    #[test]
    fn daemon_mode_arguments_oc_rsync_program() {
        let args: Vec<OsString> = vec![OsString::from("oc-rsync"), OsString::from("--daemon")];
        let result = daemon_mode_arguments(&args);
        assert!(result.is_some());
        // The first argument should be the daemon program name
        let daemon_args = result.unwrap();
        assert!(!daemon_args.is_empty());
    }

    // ==================== server_mode_requested tests ====================

    #[test]
    fn server_mode_requested_no_server_flag() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("-av"),
            OsString::from("src/"),
            OsString::from("dest/"),
        ];
        assert!(!server_mode_requested(&args));
    }

    #[test]
    fn server_mode_requested_with_server_flag() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-logDtprze.iLsfxC"),
            OsString::from("."),
            OsString::from("src/"),
        ];
        assert!(server_mode_requested(&args));
    }

    #[test]
    fn server_mode_requested_server_first_arg() {
        let args: Vec<OsString> = vec![OsString::from("rsync"), OsString::from("--server")];
        assert!(server_mode_requested(&args));
    }

    #[test]
    fn server_mode_requested_empty_args() {
        let args: Vec<OsString> = vec![];
        assert!(!server_mode_requested(&args));
    }

    #[test]
    fn server_mode_requested_program_only() {
        let args: Vec<OsString> = vec![OsString::from("rsync")];
        assert!(!server_mode_requested(&args));
    }

    #[test]
    fn server_mode_requested_with_sender() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("-logDtprze.iLsfxC"),
            OsString::from("."),
            OsString::from("src/"),
        ];
        assert!(server_mode_requested(&args));
    }

    #[test]
    fn server_mode_requested_with_receiver() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("--receiver"),
            OsString::from("-logDtprze.iLsfxC"),
            OsString::from("."),
            OsString::from("dest/"),
        ];
        assert!(server_mode_requested(&args));
    }

    #[test]
    fn server_mode_requested_server_not_in_first_position() {
        // --server can appear anywhere in args after the program name
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("-v"),
            OsString::from("--server"),
            OsString::from("-logDtprze.iLsfxC"),
        ];
        assert!(server_mode_requested(&args));
    }

    // ==================== detect_secluded_args_flag tests ====================

    #[test]
    fn detect_secluded_args_when_present() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-s"),
            OsString::from("."),
        ];
        assert!(detect_secluded_args_flag(&args));
    }

    #[test]
    fn detect_secluded_args_when_absent() {
        let args: Vec<OsString> = vec![
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("dest"),
        ];
        assert!(!detect_secluded_args_flag(&args));
    }

    #[test]
    fn detect_secluded_args_ignores_program_name() {
        // -s in program position should not be detected
        let args: Vec<OsString> = vec![OsString::from("-s"), OsString::from("--server")];
        assert!(!detect_secluded_args_flag(&args));
    }

    // ==================== parse_server_flag_string_and_args tests ====================

    #[test]
    fn parse_server_args_basic() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("dest"),
        ];
        let (flags, pos_args) = parse_server_flag_string_and_args(&args);
        assert_eq!(flags, "-logDtpr");
        assert_eq!(pos_args, vec![OsString::from("dest")]);
    }

    #[test]
    fn parse_server_args_skips_known_long_args() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("--ignore-errors"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("src/"),
        ];
        let (flags, pos_args) = parse_server_flag_string_and_args(&args);
        assert_eq!(flags, "-logDtpr");
        assert_eq!(pos_args, vec![OsString::from("src/")]);
    }

    #[test]
    fn parse_server_args_skips_secluded_flag() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("-s"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("dest"),
        ];
        let (flags, pos_args) = parse_server_flag_string_and_args(&args);
        assert_eq!(flags, "-logDtpr");
        assert_eq!(pos_args, vec![OsString::from("dest")]);
    }
}
