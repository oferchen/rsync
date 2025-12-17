#![deny(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

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

    let role = if is_sender {
        ServerRole::Generator // Server sends files to client (generator role)
    } else if is_receiver {
        ServerRole::Receiver // Server receives files from client
    } else {
        // Default to receiver if neither specified (upstream behavior)
        ServerRole::Receiver
    };

    // Extract flag string and positional arguments
    // Example args: ["oc-rsync", "--server", "--sender", "-vlogDtprze.iLsfxC.", ".", "src/"]
    // Flag string is the first arg starting with '-' after --server/--sender/--receiver
    // Everything after the flag string (and optional ".") are positional args

    let mut flag_string = String::new();
    let mut positional_args = Vec::new();
    let mut found_flags = false;

    for arg in args.iter().skip(1) {
        let arg_str = arg.to_string_lossy();

        // Skip --server, --sender, --receiver
        if arg_str == "--server" || arg_str == "--sender" || arg_str == "--receiver" {
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

    // Build server configuration
    let config = match ServerConfig::from_flag_string_and_args(role, flag_string, positional_args) {
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

    // Run native server with stdio
    let mut stdin = io::stdin().lock();

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
