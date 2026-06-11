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

    // When secluded-args is active, the client splits its argv: the
    // server-options head (--server, --sender, packed flag string, and
    // value-bearing long flags) travels on the command line, while the
    // trailing positional args (the `.` separator and path arguments)
    // stream over stdin as NUL-delimited bytes terminated by an empty
    // string. Keep the command-line argv tail and append the stdin
    // payload, skipping the synthetic "rsync" arg0 the wire prepends.
    //
    // upstream: main.c::read_args() merges cmdline args with stdin args
    // under --protect-args / secluded-args. rsync.c:283
    // send_protected_args() rewrites args[i] to "rsync" at the NULL
    // split inserted by options.c:2745; io.c:1308 read_args() then
    // re-runs parse_arguments() on the server side.
    let effective_args: Vec<OsString>;
    let effective_slice: &[OsString] = if secluded_args {
        match protocol::secluded_args::recv_secluded_args(&mut stdin, None) {
            Ok(received_args) => {
                // Discard the synthetic "rsync" arg0 from the wire and
                // prepend the command-line tail so the server-options
                // head (flag string + long flags) is in effective_args.
                let mut received_iter = received_args.into_iter();
                let _arg0 = received_iter.next();
                let cmdline_tail = args.iter().skip(1).cloned();
                effective_args = cmdline_tail
                    .chain(received_iter.map(OsString::from))
                    .collect();
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

    let long_flags = parse_server_long_flags(effective_slice);

    let (flag_string, positional_args) = parse_server_flag_string_and_args(effective_slice);

    // upstream: main.c server_sender check - default to Receiver when neither
    // --sender nor --receiver is specified.
    let role = if long_flags.is_sender {
        ServerRole::Generator
    } else {
        ServerRole::Receiver
    };

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

    // upstream: options.c parse_output_words - server-side info parsing
    // silently ignores unknown tokens so a newer client can forward names
    // this build has not learned yet. The well-formed empty/level errors
    // still surface so malformed input is not swallowed entirely.
    if !long_flags.info.is_empty() {
        match super::super::execution::parse_info_flags_server(&long_flags.info) {
            Ok(settings) => {
                // Apply resolved info levels to the thread-local config so
                // info_log! callsites on the server side respect the client's
                // --info settings.
                settings.apply_to_thread_local();
            }
            Err(message) => {
                write_server_error(stderr, program_brand, message.text().to_owned());
                return 1;
            }
        }
    }

    // Boolean and move-only flags applied after value parsing releases its borrow.
    config.deletion.ignore_errors = long_flags.ignore_errors;
    config.write.fsync = long_flags.fsync;
    config.write.io_uring_policy = long_flags.io_uring_policy;
    config.write.zero_copy_policy = long_flags.zero_copy_policy;
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
    // upstream: options.c:2046-2048 - do_stats sets info_levels[INFO_STATS] >= 2.
    // The server-side flag must be set so the generator emits NDX_DEL_STATS
    // during the goodbye phase (generator.c:2377,2422).
    config.do_stats = long_flags.stats;
    config.reference_directories = long_flags.reference_directories;

    // upstream: options.c:2327-2338 - server parses --log-format to determine
    // whether itemize data is needed. %i or %I in the format sets
    // stdout_format_has_i, which controls generator itemize output.
    if let Some(fmt) = &long_flags.log_format {
        if fmt.contains("%i") || fmt.contains("%I") {
            config.flags.info_flags.itemize = true;
        }
    }

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

    // upstream: options.c:2800-2805 - `--compress-choice`, `--new-compress`, and
    // `--old-compress` carry the explicit codec when the negotiated algorithm is
    // not the default CPRES_ZLIB. Without forwarding it into `ServerConfig`, the
    // SSH server path skips compression entirely (handshake.client_args is None
    // in SSH mode), so the receiver tries to decode upstream's compressed token
    // stream as plain tokens and eventually misaligns onto a multiplex frame
    // boundary.
    if let Some(name) = &long_flags.compress_choice {
        match protocol::CompressionAlgorithm::parse(name) {
            Ok(algo) => config.connection.compress_choice = Some(algo),
            Err(e) => {
                write_server_error(
                    stderr,
                    program_brand,
                    format!("invalid compression algorithm '{name}': {e}"),
                );
                return 1;
            }
        }
    }

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

    // upstream: options.c:1943-1950 - server-side `--max-alloc` is parsed and
    // applied to the local allocator. We forward it from the client and
    // enforce the cap on the server's buffer pool.
    if let Some(alloc_str) = &long_flags.max_alloc {
        match super::super::execution::parse_max_alloc_argument(std::ffi::OsStr::new(alloc_str)) {
            Ok(limit) => {
                if let Ok(limit_usize) = usize::try_from(limit)
                    && limit_usize > 0
                {
                    let cfg = engine::local_copy::GlobalBufferPoolConfig {
                        byte_budget: Some(limit_usize),
                        ..engine::local_copy::GlobalBufferPoolConfig::default()
                    };
                    let _ = engine::local_copy::init_global_buffer_pool(cfg);
                }
            }
            Err(message) => {
                write_server_error(stderr, brand, message.to_string());
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

    if let Some(depth_str) = &long_flags.io_uring_depth {
        match depth_str.parse::<u32>() {
            Ok(parsed) => match fast_io::validate_io_uring_depth(parsed) {
                Ok(depth) => config.write.io_uring_depth = Some(depth),
                Err(e) => {
                    write_server_error(
                        stderr,
                        brand,
                        format!("invalid --io-uring-depth value '{depth_str}': {e}"),
                    );
                    return Err(1);
                }
            },
            Err(_) => {
                write_server_error(
                    stderr,
                    brand,
                    format!("invalid --io-uring-depth value '{depth_str}'"),
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
