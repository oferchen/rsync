//! Server mode execution - orchestrates argument parsing and server startup.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

use core::branding::Brand;
use core::message::Role;
use core::rsync_error;
use logging_sink::MessageSink;

use super::flags::{detect_secluded_args_flag, parse_server_long_flags};
use super::parse::{
    parse_server_checksum_seed, parse_server_flag_string_and_args, parse_server_size_limit,
    parse_server_stop_after, parse_server_stop_at,
};

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

    let program_brand =
        super::super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    // Detect secluded-args mode: `-s` flag appears as a standalone argument
    // after --server. upstream: options.c - protect_args in server mode.
    let secluded_args = detect_secluded_args_flag(args);

    let mut stdin = io::stdin().lock();

    // When secluded-args is active, read the full argument list from stdin
    // before parsing server flags. The client sends arguments as
    // null-separated strings terminated by an empty string.
    // upstream: main.c - read_args() reads protected args from stdin.
    let effective_args: Vec<OsString>;
    let effective_slice: &[OsString] = if secluded_args {
        match protocol::secluded_args::recv_secluded_args(&mut stdin, None) {
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

    // Apply value-bearing flags, returning parse errors to the client.
    // upstream: options.c - server_options() sends these as `--flag=value`.
    if let Err(code) = apply_value_flags(&mut config, &long_flags, stderr, program_brand) {
        return code;
    }

    // Apply boolean and move flags after value parsing borrows long_flags.
    config.deletion.ignore_errors = long_flags.ignore_errors;
    config.write.fsync = long_flags.fsync;
    config.write.io_uring_policy = long_flags.io_uring_policy;
    config.write.write_devices = long_flags.write_devices;
    // upstream: options.c:2493 - server always trusts sender (am_server implies trust)
    config.trust_sender = true;
    config.qsort = long_flags.qsort;
    config.file_selection.files_from_path = long_flags.files_from;
    config.file_selection.from0 = long_flags.from0;
    config.write.inplace = long_flags.inplace;
    config.file_selection.size_only = long_flags.size_only;
    config.file_selection.ignore_existing = long_flags.ignore_existing;
    config.file_selection.existing_only = long_flags.existing_only;
    config.flags.numeric_ids = long_flags.numeric_ids;
    config.flags.delete = long_flags.delete;
    config.reference_directories = long_flags.reference_directories;

    // upstream: rsync.c:85-147 setup_iconv() - server opens iconv against the
    // wire's UTF-8 charset using the local-side spec forwarded by the client
    // (options.c:2716-2723). Without this wiring the receiver/generator skip
    // the iconv hook and write/read raw bytes verbatim, breaking transfers
    // with --iconv=LOCAL,REMOTE where the on-disk filenames differ between
    // the two sides.
    if let Some(spec) = &long_flags.iconv {
        use core::client::IconvSetting;
        match IconvSetting::parse(spec) {
            Ok(setting) => config.connection.iconv = setting.resolve_converter(),
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid --iconv value '{spec}': {e}"),
                );
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

/// Applies value-bearing flags to the server config, returning early on parse errors.
fn apply_value_flags<Err: Write>(
    config: &mut core::server::ServerConfig,
    long_flags: &super::flags::ServerLongFlags,
    stderr: &mut Err,
    brand: Brand,
) -> Result<(), i32> {
    if let Some(seed_str) = &long_flags.checksum_seed {
        match parse_server_checksum_seed(seed_str) {
            Ok(seed) => config.checksum_seed = Some(seed),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(algo_str) = &long_flags.checksum_choice {
        match protocol::ChecksumAlgorithm::parse(algo_str) {
            Ok(algo) => config.checksum_choice = Some(algo),
            Err(e) => {
                write_server_error(stderr, brand, format!("invalid --checksum-choice: {e}"));
                return Err(1);
            }
        }
    }

    if let Some(size_str) = &long_flags.min_size {
        match parse_server_size_limit(size_str, "--min-size") {
            Ok(size) => config.file_selection.min_file_size = Some(size),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(size_str) = &long_flags.max_size {
        match parse_server_size_limit(size_str, "--max-size") {
            Ok(size) => config.file_selection.max_file_size = Some(size),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(when_str) = &long_flags.stop_at {
        match parse_server_stop_at(when_str) {
            Ok(deadline) => config.stop_at = Some(deadline),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(mins_str) = &long_flags.stop_after {
        match parse_server_stop_after(mins_str) {
            Ok(deadline) => config.stop_at = Some(deadline),
            Err(msg) => {
                write_server_error(stderr, brand, msg);
                return Err(1);
            }
        }
    }

    if let Some(max_del_str) = &long_flags.max_delete {
        match max_del_str.parse::<u64>() {
            Ok(limit) => config.deletion.max_delete = Some(limit),
            Err(_) => {
                write_server_error(
                    stderr,
                    brand,
                    format!("invalid --max-delete value '{max_del_str}'"),
                );
                return Err(1);
            }
        }
    }

    Ok(())
}

fn write_server_error<Err: Write>(stderr: &mut Err, brand: Brand, text: impl fmt::Display) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Server);
    if super::super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}
