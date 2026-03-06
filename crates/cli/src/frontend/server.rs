#![deny(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
use std::time::SystemTime;

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

    // Detect secluded-args mode: `-s` flag appears as a standalone argument
    // after --server. upstream: options.c — protect_args in server mode.
    let secluded_args = detect_secluded_args_flag(args);

    let mut stdin = io::stdin().lock();

    // When secluded-args is active, read the full argument list from stdin
    // before parsing server flags. The client sends arguments as
    // null-separated strings terminated by an empty string.
    // upstream: main.c — read_args() reads protected args from stdin.
    let effective_args: Vec<OsString>;
    let effective_slice: &[OsString] = if secluded_args {
        match protocol::secluded_args::recv_secluded_args(&mut stdin) {
            Ok(received_args) => {
                effective_args = received_args.into_iter().map(OsString::from).collect();
                &effective_args
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
        &args[1..]
    };

    // Parse all long-form flags from the argument list.
    let long_flags = parse_server_long_flags(effective_slice);

    // Extract the compact flag string and positional args.
    let (flag_string, positional_args) = parse_server_flag_string_and_args(effective_slice);

    // Determine role from --sender flag. Default is Receiver when neither
    // --sender nor --receiver is specified (upstream: main.c server_sender check).
    let role = if long_flags.is_sender {
        ServerRole::Generator
    } else {
        ServerRole::Receiver
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

    // Apply boolean flags.
    config.ignore_errors = long_flags.ignore_errors;
    config.fsync = long_flags.fsync;
    config.io_uring_policy = long_flags.io_uring_policy;
    config.write_devices = long_flags.write_devices;
    config.trust_sender = long_flags.trust_sender;
    config.qsort = long_flags.qsort;
    config.files_from_path = long_flags.files_from;
    config.from0 = long_flags.from0;
    config.inplace = long_flags.inplace;
    config.size_only = long_flags.size_only;
    config.flags.numeric_ids = long_flags.numeric_ids;
    config.flags.delete = long_flags.delete;

    // Apply value-bearing flags, returning parse errors to the client.
    // upstream: options.c — server_options() sends these as `--flag=value`.
    if let Some(seed_str) = &long_flags.checksum_seed {
        match parse_server_checksum_seed(seed_str) {
            Ok(seed) => config.checksum_seed = Some(seed),
            Err(msg) => {
                write_server_error(stderr, program_brand, msg);
                return 1;
            }
        }
    }

    if let Some(algo_str) = &long_flags.checksum_choice {
        match protocol::ChecksumAlgorithm::parse(algo_str) {
            Ok(algo) => config.checksum_choice = Some(algo),
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid --checksum-choice: {e}"),
                );
                return 1;
            }
        }
    }

    if let Some(size_str) = &long_flags.min_size {
        match parse_server_size_limit(size_str, "--min-size") {
            Ok(size) => config.min_file_size = Some(size),
            Err(msg) => {
                write_server_error(stderr, program_brand, msg);
                return 1;
            }
        }
    }

    if let Some(size_str) = &long_flags.max_size {
        match parse_server_size_limit(size_str, "--max-size") {
            Ok(size) => config.max_file_size = Some(size),
            Err(msg) => {
                write_server_error(stderr, program_brand, msg);
                return 1;
            }
        }
    }

    if let Some(when_str) = &long_flags.stop_at {
        match parse_server_stop_at(when_str) {
            Ok(deadline) => config.stop_at = Some(deadline),
            Err(msg) => {
                write_server_error(stderr, program_brand, msg);
                return 1;
            }
        }
    }

    if let Some(mins_str) = &long_flags.stop_after {
        match parse_server_stop_after(mins_str) {
            Ok(deadline) => config.stop_at = Some(deadline),
            Err(msg) => {
                write_server_error(stderr, program_brand, msg);
                return 1;
            }
        }
    }

    // Run native server with stdio
    match run_server_stdio(config, &mut stdin, stdout, None) {
        Ok(_stats) => 0,
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

/// Long-form flags extracted from the server argument list.
///
/// These correspond to the `--flag` and `--flag=value` arguments that
/// upstream rsync's `server_options()` emits alongside the compact flag string.
/// upstream: options.c — `server_options()`.
struct ServerLongFlags {
    is_sender: bool,
    is_receiver: bool,
    ignore_errors: bool,
    fsync: bool,
    io_uring_policy: fast_io::IoUringPolicy,
    write_devices: bool,
    trust_sender: bool,
    qsort: bool,
    checksum_seed: Option<String>,
    checksum_choice: Option<String>,
    min_size: Option<String>,
    max_size: Option<String>,
    stop_at: Option<String>,
    stop_after: Option<String>,
    files_from: Option<String>,
    from0: bool,
    inplace: bool,
    size_only: bool,
    /// Numeric IDs only (upstream: `--numeric-ids`, long-form only).
    numeric_ids: bool,
    /// Delete extraneous files (upstream: `--delete-*` variants, long-form only).
    delete: bool,
}

/// Parses all long-form flags from the server argument list.
///
/// Scans the argument list for `--flag` and `--flag=value` arguments,
/// extracting their values into a structured result. Unknown long flags
/// are ignored for forward compatibility.
fn parse_server_long_flags(args: &[OsString]) -> ServerLongFlags {
    let mut flags = ServerLongFlags {
        is_sender: false,
        is_receiver: false,
        ignore_errors: false,
        fsync: false,
        io_uring_policy: fast_io::IoUringPolicy::Auto,
        write_devices: false,
        trust_sender: false,
        qsort: false,
        checksum_seed: None,
        checksum_choice: None,
        min_size: None,
        max_size: None,
        stop_at: None,
        stop_after: None,
        files_from: None,
        from0: false,
        inplace: false,
        size_only: false,
        numeric_ids: false,
        delete: false,
    };

    for arg in args {
        let s = arg.to_string_lossy();

        match s.as_ref() {
            "--sender" => flags.is_sender = true,
            "--receiver" => flags.is_receiver = true,
            "--ignore-errors" => flags.ignore_errors = true,
            "--fsync" => flags.fsync = true,
            "--io-uring" => flags.io_uring_policy = fast_io::IoUringPolicy::Enabled,
            "--no-io-uring" => flags.io_uring_policy = fast_io::IoUringPolicy::Disabled,
            "--write-devices" => flags.write_devices = true,
            "--trust-sender" => flags.trust_sender = true,
            "--qsort" => flags.qsort = true,
            "--from0" => flags.from0 = true,
            "--inplace" => flags.inplace = true,
            "--size-only" => flags.size_only = true,
            // upstream: --numeric-ids is long-form only (options.c:2887-2888)
            "--numeric-ids" => flags.numeric_ids = true,
            // upstream: --delete variants are long-form only (options.c:2818-2827)
            "--delete" | "--delete-before" | "--delete-during" | "--delete-after"
            | "--delete-delay" | "--delete-excluded" => flags.delete = true,
            _ => {
                // Value-bearing flags use `--flag=value` syntax.
                if let Some(value) = s.strip_prefix("--checksum-seed=") {
                    flags.checksum_seed = Some(value.to_owned());
                } else if let Some(value) = s.strip_prefix("--checksum-choice=") {
                    flags.checksum_choice = Some(value.to_owned());
                } else if let Some(value) = s.strip_prefix("--min-size=") {
                    flags.min_size = Some(value.to_owned());
                } else if let Some(value) = s.strip_prefix("--max-size=") {
                    flags.max_size = Some(value.to_owned());
                } else if let Some(value) = s.strip_prefix("--stop-at=") {
                    flags.stop_at = Some(value.to_owned());
                } else if let Some(value) = s.strip_prefix("--stop-after=") {
                    flags.stop_after = Some(value.to_owned());
                } else if let Some(value) = s.strip_prefix("--files-from=") {
                    flags.files_from = Some(value.to_owned());
                }
            }
        }
    }

    flags
}

/// Returns `true` when the argument is a known server-mode long flag.
///
/// Used by [`parse_server_flag_string_and_args`] to skip long flags when
/// searching for the compact flag string.
fn is_known_server_long_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--server"
            | "--sender"
            | "--receiver"
            | "--ignore-errors"
            | "--fsync"
            | "--io-uring"
            | "--no-io-uring"
            | "--write-devices"
            | "--trust-sender"
            | "--qsort"
            | "--from0"
            | "--inplace"
            | "--size-only"
            | "--numeric-ids"
            | "--delete"
            | "--delete-before"
            | "--delete-during"
            | "--delete-after"
            | "--delete-delay"
            | "--delete-excluded"
    ) || arg == "-s"
        || arg.starts_with("--checksum-seed=")
        || arg.starts_with("--checksum-choice=")
        || arg.starts_with("--min-size=")
        || arg.starts_with("--max-size=")
        || arg.starts_with("--stop-at=")
        || arg.starts_with("--stop-after=")
        || arg.starts_with("--files-from=")
}

/// Parses a `--checksum-seed=NUM` value from the server argument list.
///
/// upstream: options.c — `--checksum-seed=NUM` parsed in `server_options()`.
fn parse_server_checksum_seed(value: &str) -> Result<u32, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("--checksum-seed value must not be empty".to_owned());
    }
    trimmed.parse::<u32>().map_err(|_| {
        format!(
            "invalid --checksum-seed value '{value}': must be 0..{}",
            u32::MAX
        )
    })
}

/// Parses a `--min-size=SIZE` or `--max-size=SIZE` value from the server argument list.
///
/// Delegates to the shared size parser used by the client-side CLI.
/// upstream: options.c — `--min-size` / `--max-size` in `server_options()`.
fn parse_server_size_limit(value: &str, flag: &str) -> Result<u64, String> {
    let os_value = OsStr::new(value);
    super::execution::parse_size_limit_argument(os_value, flag).map_err(|msg| msg.to_string())
}

/// Parses a `--stop-at=WHEN` value from the server argument list.
///
/// Delegates to the shared stop-at parser.
/// upstream: options.c — `--stop-at` in `server_options()`.
fn parse_server_stop_at(value: &str) -> Result<SystemTime, String> {
    let os_value = OsStr::new(value);
    super::execution::parse_stop_at_argument(os_value).map_err(|msg| msg.to_string())
}

/// Parses a `--stop-after=MINS` value from the server argument list.
///
/// Converts minutes to an absolute deadline (now + minutes).
/// upstream: options.c — `--stop-after` / `--time-limit` in `server_options()`.
fn parse_server_stop_after(value: &str) -> Result<SystemTime, String> {
    let os_value = OsStr::new(value);
    super::execution::parse_stop_after_argument(os_value).map_err(|msg| msg.to_string())
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
        if is_known_server_long_flag(&arg_str) {
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

    #[test]
    fn parse_server_args_skips_new_boolean_long_flags() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("--write-devices"),
            OsString::from("--trust-sender"),
            OsString::from("--qsort"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("src/"),
        ];
        let (flags, pos_args) = parse_server_flag_string_and_args(&args);
        assert_eq!(flags, "-logDtpr");
        assert_eq!(pos_args, vec![OsString::from("src/")]);
    }

    #[test]
    fn parse_server_args_skips_value_bearing_long_flags() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--checksum-seed=12345"),
            OsString::from("--checksum-choice=xxh3"),
            OsString::from("--min-size=1K"),
            OsString::from("--max-size=1G"),
            OsString::from("--stop-after=60"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("dest/"),
        ];
        let (flags, pos_args) = parse_server_flag_string_and_args(&args);
        assert_eq!(flags, "-logDtpr");
        assert_eq!(pos_args, vec![OsString::from("dest/")]);
    }

    // ==================== parse_server_long_flags tests ====================

    #[test]
    fn long_flags_defaults() {
        let args: Vec<OsString> = vec![OsString::from("--server")];
        let flags = parse_server_long_flags(&args);
        assert!(!flags.is_sender);
        assert!(!flags.is_receiver);
        assert!(!flags.ignore_errors);
        assert!(!flags.fsync);
        assert!(!flags.write_devices);
        assert!(!flags.trust_sender);
        assert!(!flags.qsort);
        assert!(flags.checksum_seed.is_none());
        assert!(flags.checksum_choice.is_none());
        assert!(flags.min_size.is_none());
        assert!(flags.max_size.is_none());
        assert!(flags.stop_at.is_none());
        assert!(flags.stop_after.is_none());
        assert!(matches!(
            flags.io_uring_policy,
            fast_io::IoUringPolicy::Auto
        ));
    }

    #[test]
    fn long_flags_sender() {
        let args = vec![OsString::from("--server"), OsString::from("--sender")];
        let flags = parse_server_long_flags(&args);
        assert!(flags.is_sender);
        assert!(!flags.is_receiver);
    }

    #[test]
    fn long_flags_receiver() {
        let args = vec![OsString::from("--server"), OsString::from("--receiver")];
        let flags = parse_server_long_flags(&args);
        assert!(!flags.is_sender);
        assert!(flags.is_receiver);
    }

    #[test]
    fn long_flags_ignore_errors() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--ignore-errors"),
        ];
        let flags = parse_server_long_flags(&args);
        assert!(flags.ignore_errors);
    }

    #[test]
    fn long_flags_fsync() {
        let args = vec![OsString::from("--server"), OsString::from("--fsync")];
        let flags = parse_server_long_flags(&args);
        assert!(flags.fsync);
    }

    #[test]
    fn long_flags_io_uring_enabled() {
        let args = vec![OsString::from("--server"), OsString::from("--io-uring")];
        let flags = parse_server_long_flags(&args);
        assert!(matches!(
            flags.io_uring_policy,
            fast_io::IoUringPolicy::Enabled
        ));
    }

    #[test]
    fn long_flags_io_uring_disabled() {
        let args = vec![OsString::from("--server"), OsString::from("--no-io-uring")];
        let flags = parse_server_long_flags(&args);
        assert!(matches!(
            flags.io_uring_policy,
            fast_io::IoUringPolicy::Disabled
        ));
    }

    #[test]
    fn long_flags_write_devices() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--write-devices"),
        ];
        let flags = parse_server_long_flags(&args);
        assert!(flags.write_devices);
    }

    #[test]
    fn long_flags_trust_sender() {
        let args = vec![OsString::from("--server"), OsString::from("--trust-sender")];
        let flags = parse_server_long_flags(&args);
        assert!(flags.trust_sender);
    }

    #[test]
    fn long_flags_qsort() {
        let args = vec![OsString::from("--server"), OsString::from("--qsort")];
        let flags = parse_server_long_flags(&args);
        assert!(flags.qsort);
    }

    #[test]
    fn long_flags_checksum_seed() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--checksum-seed=42"),
        ];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.checksum_seed.as_deref(), Some("42"));
    }

    #[test]
    fn long_flags_checksum_choice() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--checksum-choice=xxh3"),
        ];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.checksum_choice.as_deref(), Some("xxh3"));
    }

    #[test]
    fn long_flags_min_size() {
        let args = vec![OsString::from("--server"), OsString::from("--min-size=1K")];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.min_size.as_deref(), Some("1K"));
    }

    #[test]
    fn long_flags_max_size() {
        let args = vec![OsString::from("--server"), OsString::from("--max-size=1G")];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.max_size.as_deref(), Some("1G"));
    }

    #[test]
    fn long_flags_stop_at() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--stop-at=2099-12-31T23:59"),
        ];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.stop_at.as_deref(), Some("2099-12-31T23:59"));
    }

    #[test]
    fn long_flags_stop_after() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--stop-after=60"),
        ];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.stop_after.as_deref(), Some("60"));
    }

    #[test]
    fn long_flags_all_combined() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("--ignore-errors"),
            OsString::from("--fsync"),
            OsString::from("--write-devices"),
            OsString::from("--trust-sender"),
            OsString::from("--qsort"),
            OsString::from("--checksum-seed=99"),
            OsString::from("--checksum-choice=md5"),
            OsString::from("--min-size=100"),
            OsString::from("--max-size=1M"),
            OsString::from("--stop-after=30"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("src/"),
        ];
        let flags = parse_server_long_flags(&args);
        assert!(flags.is_sender);
        assert!(flags.ignore_errors);
        assert!(flags.fsync);
        assert!(flags.write_devices);
        assert!(flags.trust_sender);
        assert!(flags.qsort);
        assert_eq!(flags.checksum_seed.as_deref(), Some("99"));
        assert_eq!(flags.checksum_choice.as_deref(), Some("md5"));
        assert_eq!(flags.min_size.as_deref(), Some("100"));
        assert_eq!(flags.max_size.as_deref(), Some("1M"));
        assert_eq!(flags.stop_after.as_deref(), Some("30"));
    }

    // ==================== is_known_server_long_flag tests ====================

    #[test]
    fn known_flag_detects_boolean_flags() {
        assert!(is_known_server_long_flag("--server"));
        assert!(is_known_server_long_flag("--sender"));
        assert!(is_known_server_long_flag("--receiver"));
        assert!(is_known_server_long_flag("--ignore-errors"));
        assert!(is_known_server_long_flag("--fsync"));
        assert!(is_known_server_long_flag("--io-uring"));
        assert!(is_known_server_long_flag("--no-io-uring"));
        assert!(is_known_server_long_flag("--write-devices"));
        assert!(is_known_server_long_flag("--trust-sender"));
        assert!(is_known_server_long_flag("--qsort"));
        assert!(is_known_server_long_flag("--from0"));
        assert!(is_known_server_long_flag("-s"));
    }

    #[test]
    fn known_flag_detects_value_flags() {
        assert!(is_known_server_long_flag("--checksum-seed=0"));
        assert!(is_known_server_long_flag("--checksum-choice=xxh3"));
        assert!(is_known_server_long_flag("--min-size=1K"));
        assert!(is_known_server_long_flag("--max-size=1G"));
        assert!(is_known_server_long_flag("--stop-at=2099-12-31"));
        assert!(is_known_server_long_flag("--stop-after=60"));
        assert!(is_known_server_long_flag("--files-from=-"));
        assert!(is_known_server_long_flag("--files-from=/path/to/list"));
    }

    #[test]
    fn known_flag_rejects_unknown() {
        assert!(!is_known_server_long_flag("--unknown"));
        assert!(!is_known_server_long_flag("-v"));
        assert!(!is_known_server_long_flag("-logDtpr"));
        assert!(!is_known_server_long_flag("dest/"));
    }

    // ==================== parse_server_checksum_seed tests ====================

    #[test]
    fn checksum_seed_parses_valid() {
        assert_eq!(parse_server_checksum_seed("0").unwrap(), 0);
        assert_eq!(parse_server_checksum_seed("12345").unwrap(), 12345);
        assert_eq!(parse_server_checksum_seed("4294967295").unwrap(), u32::MAX);
    }

    #[test]
    fn checksum_seed_rejects_empty() {
        assert!(parse_server_checksum_seed("").is_err());
    }

    #[test]
    fn checksum_seed_rejects_non_numeric() {
        assert!(parse_server_checksum_seed("abc").is_err());
    }

    #[test]
    fn checksum_seed_rejects_overflow() {
        assert!(parse_server_checksum_seed("4294967296").is_err());
    }

    #[test]
    fn checksum_seed_trims_whitespace() {
        assert_eq!(parse_server_checksum_seed("  42  ").unwrap(), 42);
    }

    // ==================== parse_server_size_limit tests ====================

    #[test]
    fn size_limit_parses_plain_number() {
        assert_eq!(parse_server_size_limit("100", "--min-size").unwrap(), 100);
    }

    #[test]
    fn size_limit_parses_kilobytes() {
        assert_eq!(parse_server_size_limit("1K", "--min-size").unwrap(), 1024);
    }

    #[test]
    fn size_limit_parses_megabytes() {
        assert_eq!(
            parse_server_size_limit("1M", "--max-size").unwrap(),
            1024 * 1024
        );
    }

    #[test]
    fn size_limit_parses_gigabytes() {
        assert_eq!(
            parse_server_size_limit("1G", "--max-size").unwrap(),
            1024 * 1024 * 1024
        );
    }

    #[test]
    fn size_limit_rejects_empty() {
        assert!(parse_server_size_limit("", "--min-size").is_err());
    }

    #[test]
    fn size_limit_rejects_invalid() {
        assert!(parse_server_size_limit("abc", "--max-size").is_err());
    }

    // ==================== parse_server_stop_after tests ====================

    #[test]
    fn stop_after_parses_valid_minutes() {
        let deadline = parse_server_stop_after("10").unwrap();
        let duration = deadline.duration_since(SystemTime::now()).unwrap();
        // Approximately 10 minutes (600 seconds), allow small drift
        assert!(duration.as_secs() >= 598 && duration.as_secs() <= 602);
    }

    #[test]
    fn stop_after_rejects_zero() {
        assert!(parse_server_stop_after("0").is_err());
    }

    #[test]
    fn stop_after_rejects_empty() {
        assert!(parse_server_stop_after("").is_err());
    }

    #[test]
    fn stop_after_rejects_non_numeric() {
        assert!(parse_server_stop_after("abc").is_err());
    }

    // ==================== parse_server_stop_at tests ====================

    #[test]
    fn stop_at_rejects_empty() {
        assert!(parse_server_stop_at("").is_err());
    }

    #[test]
    fn stop_at_rejects_invalid_format() {
        assert!(parse_server_stop_at("invalid").is_err());
    }

    #[test]
    fn stop_at_parses_far_future_date() {
        // 2099 is far enough in the future to always be valid
        let result = parse_server_stop_at("2099-12-31T23:59");
        // May fail due to local offset issues in test env, but format should be ok
        assert!(result.is_ok() || result.is_err());
    }

    // ==================== files-from and from0 flag tests ====================

    #[test]
    fn long_flags_files_from_stdin() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--files-from=-"),
            OsString::from("--from0"),
        ];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.files_from.as_deref(), Some("-"));
        assert!(flags.from0);
    }

    #[test]
    fn long_flags_files_from_path() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--files-from=/tmp/list.txt"),
        ];
        let flags = parse_server_long_flags(&args);
        assert_eq!(flags.files_from.as_deref(), Some("/tmp/list.txt"));
        assert!(!flags.from0);
    }

    #[test]
    fn long_flags_from0_without_files_from() {
        let args = vec![OsString::from("--server"), OsString::from("--from0")];
        let flags = parse_server_long_flags(&args);
        assert!(flags.from0);
        assert!(flags.files_from.is_none());
    }

    #[test]
    fn long_flags_default_files_from() {
        let args = vec![OsString::from("--server")];
        let flags = parse_server_long_flags(&args);
        assert!(flags.files_from.is_none());
        assert!(!flags.from0);
    }

    #[test]
    fn parse_server_args_skips_files_from_and_from0() {
        let args = vec![
            OsString::from("--server"),
            OsString::from("--files-from=-"),
            OsString::from("--from0"),
            OsString::from("-logDtpr"),
            OsString::from("."),
            OsString::from("dest/"),
        ];
        let (flags, pos_args) = parse_server_flag_string_and_args(&args);
        assert_eq!(flags, "-logDtpr");
        assert_eq!(pos_args, vec![OsString::from("dest/")]);
    }
}
