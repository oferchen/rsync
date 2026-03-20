//! Transfer orchestration, argument building, and execution.
//!
//! Builds the daemon argument list (mirroring upstream `server_options()` in
//! `options.c`), configures the server infrastructure for pull/push transfers,
//! and converts server statistics to client summaries.

use std::ffi::OsString;
use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use protocol::ProtocolVersion;
use protocol::filters::FilterRuleWireFormat;

use super::super::super::config::{ClientConfig, DeleteMode, ReferenceDirectoryKind};
use super::super::super::error::{ClientError, invalid_argument_error, socket_error};
use super::super::super::progress::ClientProgressObserver;
use super::super::super::summary::ClientSummary;
use super::super::flags;
use super::connection::DaemonTransferRequest;
use transfer::setup::build_capability_string;

use crate::server::handshake::HandshakeResult;
use crate::server::{ServerConfig, ServerRole};

/// Sends daemon-mode arguments to the server.
///
/// When `--protect-args` / `-s` is active, uses a two-phase protocol
/// matching upstream `clientserver.c:393-408`:
/// - Phase 1: minimal args (`--server [-s] .`) so the daemon knows to
///   expect protected args
/// - Phase 2: full argument list via `send_secluded_args()` wire format
///
/// Without protect-args, sends all arguments in a single phase.
/// For protocol >= 30, strings are null-terminated; for < 30, newline-terminated.
pub(crate) fn send_daemon_arguments(
    stream: &mut TcpStream,
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Result<(), ClientError> {
    let protect = config.protect_args().unwrap_or(false);

    let full_args = build_full_daemon_args(config, request, protocol, is_sender);

    // Phase 1: send args over the daemon text protocol.
    // With protect-args, only send the minimal set so the daemon detects `-s`
    // and expects a phase-2 secluded-args payload.
    // upstream: clientserver.c:393-405 - stops at the NULL marker in sargs
    let phase1_args = if protect {
        build_minimal_daemon_args(is_sender)
    } else {
        full_args.clone()
    };

    let terminator = if protocol.as_u8() >= 30 { b'\0' } else { b'\n' };

    for arg in &phase1_args {
        stream.write_all(arg.as_bytes()).map_err(|e| {
            socket_error("send argument to", request.address.socket_addr_display(), e)
        })?;
        stream.write_all(&[terminator]).map_err(|e| {
            socket_error(
                "send terminator to",
                request.address.socket_addr_display(),
                e,
            )
        })?;
    }

    // Empty string signals end of phase-1 argument list.
    stream.write_all(&[terminator]).map_err(|e| {
        socket_error(
            "send final terminator to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    // Phase 2: when protect-args is active, send the real arguments via
    // the secluded-args wire format (null-separated with empty terminator).
    // upstream: clientserver.c:407-408 - send_protected_args(f_out, sargs)
    if protect {
        let mut secluded = vec!["rsync"];
        secluded.extend(full_args.iter().map(String::as_str));
        protocol::secluded_args::send_secluded_args(stream, &secluded).map_err(|e| {
            socket_error(
                "send secluded args to",
                request.address.socket_addr_display(),
                e,
            )
        })?;
    }

    stream.flush().map_err(|e| {
        socket_error(
            "flush arguments to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    Ok(())
}

/// Builds the minimal phase-1 argument list for protect-args daemon mode.
///
/// The daemon only needs `--server [--sender] -s .` to know that
/// secluded args follow in phase 2.
///
/// upstream: clientserver.c:393-405 - sargs has a NULL marker after `-s .`
fn build_minimal_daemon_args(is_sender: bool) -> Vec<String> {
    let mut args = vec!["--server".to_owned()];
    if is_sender {
        args.push("--sender".to_owned());
    }
    args.push("-s".to_owned());
    args.push(".".to_owned());
    args
}

/// Builds the full argument list for daemon-mode transfer.
///
/// Mirrors upstream `server_options()` (`options.c:2590-2997`) which builds
/// the argument list sent from client to server.
///
/// In upstream, `am_sender` refers to the CLIENT being the sender (push).
/// In our code, `is_sender` means "daemon is sender" (pull). So upstream's
/// `am_sender` corresponds to `!is_sender` here.
fn build_full_daemon_args(
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    // upstream: options.c:2590-2592
    args.push("--server".to_owned());
    if is_sender {
        args.push("--sender".to_owned());
    }

    // upstream: options.c:2797-2798
    let checksum_choice = config.checksum_choice();
    if let Some(override_algo) = checksum_choice.transfer_protocol_override() {
        args.push(format!("--checksum-choice={}", override_algo.as_str()));
    }

    // Single-character flag string (e.g., "-logDtprzc").
    // upstream: options.c:2594-2713
    let flag_string = flags::build_server_flag_string(config);
    if !flag_string.is_empty() {
        args.push(flag_string);
    }

    // Capability flags for protocol 30+.
    // upstream: options.c:2707-2713 (via maybe_add_e_option appended to argstr)
    //
    // Advertise 'i' (INC_RECURSE) only for pull direction (is_sender=true,
    // meaning daemon is the sender). Sender-side INC_RECURSE file list
    // partitioning does not yet produce correctly grouped sub-lists that
    // upstream's strict dirname validation in flist.c:receive_file_entry()
    // requires, so we disable it for push (is_sender=false) until the
    // sender emits proper incremental directory segments.
    // upstream: compat.c:720 set_allow_inc_recurse()
    if protocol.as_u8() >= 30 {
        args.push(build_capability_string(is_sender));
    }

    // --- Long-form arguments (upstream server_options() options.c:2737-2980) ---
    let we_are_sender = !is_sender;

    // --compress-level=N
    // upstream: options.c:2737-2740
    if let Some(level) = config.compression_level() {
        args.push(format!(
            "--compress-level={}",
            compression_level_numeric(level)
        ));
    }

    // Sender-specific args
    // upstream: options.c:2807-2839
    if we_are_sender {
        if let Some(max_delete) = config.max_delete() {
            if max_delete > 0 {
                args.push(format!("--max-delete={max_delete}"));
            } else {
                args.push("--max-delete=-1".to_owned());
            }
        }

        // upstream: options.c:2818-2829
        match config.delete_mode() {
            DeleteMode::Before => args.push("--delete-before".to_owned()),
            DeleteMode::Delay => args.push("--delete-delay".to_owned()),
            DeleteMode::During => args.push("--delete-during".to_owned()),
            DeleteMode::After => args.push("--delete-after".to_owned()),
            DeleteMode::Disabled => {}
        }
        if config.delete_excluded() {
            args.push("--delete-excluded".to_owned());
        }
        if config.force_replacements() {
            args.push("--force".to_owned());
        }

        // upstream: options.c:2836-2837
        if config.size_only() {
            args.push("--size-only".to_owned());
        }
    }

    // upstream: options.c:2878-2879
    if config.ignore_errors() {
        args.push("--ignore-errors".to_owned());
    }

    // upstream: options.c:2881-2882
    if config.copy_unsafe_links() {
        args.push("--copy-unsafe-links".to_owned());
    }

    // upstream: options.c:2884-2885
    if config.safe_links() {
        args.push("--safe-links".to_owned());
    }

    // upstream: options.c:2887-2888
    if config.numeric_ids() {
        args.push("--numeric-ids".to_owned());
    }

    // upstream: options.c:2890-2891
    if config.qsort() {
        args.push("--use-qsort".to_owned());
    }

    // Sender-only long-form args
    // upstream: options.c:2893-2925
    if we_are_sender {
        if config.ignore_existing() {
            args.push("--ignore-existing".to_owned());
        }
        if config.existing_only() {
            args.push("--existing".to_owned());
        }
        if config.fsync() {
            args.push("--fsync".to_owned());
        }

        // --compare-dest=DIR, --copy-dest=DIR, --link-dest=DIR
        // upstream: options.c:2915-2923 - sent only when client is sender (push).
        for ref_dir in config.reference_directories() {
            let flag = match ref_dir.kind() {
                ReferenceDirectoryKind::Compare => "--compare-dest=",
                ReferenceDirectoryKind::Copy => "--copy-dest=",
                ReferenceDirectoryKind::Link => "--link-dest=",
            };
            args.push(format!("{flag}{}", ref_dir.path().display()));
        }
    }

    // upstream: options.c:2933-2942
    if config.append() {
        args.push("--append".to_owned());
        if config.append_verify() {
            args.push("--append".to_owned());
        }
    } else if config.inplace() {
        args.push("--inplace".to_owned());
    }

    // upstream: options.c:2787-2795
    if config.backup() {
        args.push("--backup".to_owned());
        if let Some(dir) = config.backup_directory() {
            args.push("--backup-dir".to_owned());
            args.push(dir.display().to_string());
        }
        if let Some(suffix) = config.backup_suffix() {
            args.push(format!("--suffix={}", suffix.to_string_lossy()));
        }
    }

    // upstream: options.c:2964-2965
    if config.remove_source_files() {
        args.push("--remove-source-files".to_owned());
    }

    // --files-from
    // upstream: options.c:2944-2956
    {
        use super::super::super::config::FilesFromSource;
        let client_is_sender = !is_sender;
        match config.files_from() {
            FilesFromSource::None => {}
            FilesFromSource::Stdin | FilesFromSource::LocalFile(_) => {
                if !client_is_sender {
                    // Pull: daemon is sender and needs the file list from us.
                    args.push("--files-from=-".to_owned());
                    args.push("--from0".to_owned());
                }
            }
            FilesFromSource::RemoteFile(path) => {
                args.push(format!("--files-from={path}"));
                if config.from0() {
                    args.push("--from0".to_owned());
                }
            }
        }
    }

    // Dummy argument (upstream requirement - represents CWD)
    args.push(".".to_owned());

    // Module path
    let module_path = format!("{}/{}", request.module, request.path);
    args.push(module_path);

    args
}

/// Converts a [`compress::zlib::CompressionLevel`] to its numeric zlib value.
fn compression_level_numeric(level: compress::zlib::CompressionLevel) -> u32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => u32::from(n.get()),
    }
}

/// Reads the `--files-from` source and serializes it into the wire format
/// for forwarding to a remote daemon.
///
/// Handles both `Stdin` (reads from standard input) and `LocalFile` (reads
/// from the given path). The output is NUL-separated filenames terminated
/// by a double-NUL sentinel, matching the format expected by
/// [`protocol::read_files_from_stream`] on the remote side.
///
/// The `--from0` flag controls whether the input is already NUL-delimited.
///
/// # Upstream Reference
///
/// - `io.c:forward_filesfrom_data()` - reads from local fd, writes to socket
/// - `main.c:1354-1356` - `start_filesfrom_forwarding(filesfrom_fd)`
fn read_files_from_for_forwarding(config: &ClientConfig) -> Result<Vec<u8>, ClientError> {
    use super::super::super::config::FilesFromSource;

    let eol_nulls = config.from0();
    let mut wire_data = Vec::new();

    match config.files_from() {
        FilesFromSource::Stdin => {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            protocol::forward_files_from(&mut reader, &mut wire_data, eol_nulls).map_err(|e| {
                invalid_argument_error(&format!("failed to read --files-from stdin: {e}"), 23)
            })?;
        }
        FilesFromSource::LocalFile(path) => {
            let mut file = std::fs::File::open(path).map_err(|e| {
                invalid_argument_error(
                    &format!("failed to open --files-from {}: {e}", path.display()),
                    23,
                )
            })?;
            protocol::forward_files_from(&mut file, &mut wire_data, eol_nulls).map_err(|e| {
                invalid_argument_error(
                    &format!("failed to read --files-from {}: {e}", path.display()),
                    23,
                )
            })?;
        }
        FilesFromSource::None | FilesFromSource::RemoteFile(_) => {}
    }

    Ok(wire_data)
}

/// Executes a pull transfer (remote to local).
///
/// The local side acts as the receiver and the remote side acts as the
/// sender/generator. Reuses the server receiver infrastructure.
///
/// Protocol sequence (mirrors upstream `client_run` for `!am_sender`):
/// 1. Protocol already negotiated via `@RSYNCD` text exchange (not binary 4-byte)
/// 2. `setup_protocol()` does compat flags + checksum seed (NO version exchange)
/// 3. `io_start_multiplex_out()` activates output multiplex
/// 4. `send_filter_list()` sends filters after multiplex activation
/// 5. File list exchange and transfer
pub(crate) fn run_pull_transfer(
    config: &ClientConfig,
    mut stream: TcpStream,
    local_paths: &[String],
    protocol: ProtocolVersion,
) -> Result<ClientSummary, ClientError> {
    stream
        .set_nodelay(true)
        .map_err(|e| socket_error("set nodelay on", "daemon socket", e))?;

    // Replace the handshake-phase socket timeout with the user-configured --timeout.
    // upstream: io.c - select_timeout() uses io_timeout for all transfer I/O.
    let transfer_timeout = config
        .timeout()
        .as_seconds()
        .map(|s| Duration::from_secs(s.get()));
    stream
        .set_read_timeout(transfer_timeout)
        .map_err(|e| socket_error("set read timeout on", "daemon socket", e))?;
    stream
        .set_write_timeout(transfer_timeout)
        .map_err(|e| socket_error("set write timeout on", "daemon socket", e))?;

    let filter_rules = flags::build_wire_format_rules(config.filter_rules())?;

    // Protocol was negotiated via @RSYNCD text exchange, not binary 4-byte exchange.
    // setup_protocol() will skip the binary exchange because remote_protocol != 0
    // upstream: compat.c:599 - if (remote_protocol == 0) { ... }
    let handshake = HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: config.timeout().as_seconds().map(|s| s.get()),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };

    let mut server_config = build_server_config_for_receiver(config, local_paths, filter_rules)?;

    // upstream: main.c:1354-1356 - when pulling with --files-from pointing to
    // a local file or stdin, the client reads the file list locally and forwards
    // it to the daemon's generator over the protocol stream.
    if config.files_from().is_local_forwarded() {
        let data = read_files_from_for_forwarding(config)?;
        server_config.connection.files_from_data = Some(data);
    }

    let start = Instant::now();
    let server_stats =
        run_server_with_handshake_over_stream(server_config, handshake, &mut stream, None)?;
    let elapsed = start.elapsed();

    Ok(convert_server_stats_to_summary(server_stats, elapsed))
}

/// Executes a push transfer (local to remote).
///
/// The local side acts as the sender/generator and the remote side acts as the
/// receiver. Reuses the server generator infrastructure.
///
/// Protocol sequence (mirrors upstream `client_run` for `am_sender`):
/// 1. Protocol already negotiated via `@RSYNCD` text exchange (not binary 4-byte)
/// 2. `setup_protocol()` does compat flags + checksum seed (NO version exchange)
/// 3. `io_start_multiplex_out()` activates output multiplex
/// 4. `send_filter_list()` sends filters after multiplex activation
/// 5. File list exchange and transfer
pub(crate) fn run_push_transfer(
    config: &ClientConfig,
    mut stream: TcpStream,
    local_paths: &[String],
    protocol: ProtocolVersion,
) -> Result<ClientSummary, ClientError> {
    stream
        .set_nodelay(true)
        .map_err(|e| socket_error("set nodelay on", "daemon socket", e))?;

    let transfer_timeout = config
        .timeout()
        .as_seconds()
        .map(|s| Duration::from_secs(s.get()));
    stream
        .set_read_timeout(transfer_timeout)
        .map_err(|e| socket_error("set read timeout on", "daemon socket", e))?;
    stream
        .set_write_timeout(transfer_timeout)
        .map_err(|e| socket_error("set write timeout on", "daemon socket", e))?;

    let filter_rules = flags::build_wire_format_rules(config.filter_rules())?;

    // upstream: compat.c:599 - if (remote_protocol == 0) { ... }
    let handshake = HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: config.timeout().as_seconds().map(|s| s.get()),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };

    let server_config = build_server_config_for_generator(config, local_paths, filter_rules)?;
    let dry_run = config.dry_run();
    let start = Instant::now();

    // Call the server directly (not the error-wrapping helper) so we can
    // inspect the raw io::Error kind for dry-run graceful close handling.
    let mut reader = stream
        .try_clone()
        .map_err(|e| invalid_argument_error(&format!("failed to clone stream: {e}"), 23))?;

    let result = crate::server::run_server_with_handshake(
        server_config,
        handshake,
        &mut reader,
        &mut stream,
        None,
    );

    match result {
        Ok(server_stats) => {
            let elapsed = start.elapsed();
            Ok(convert_server_stats_to_summary(server_stats, elapsed))
        }
        Err(ref e) if dry_run && is_dry_run_remote_close(e) => {
            // upstream: clientserver.c - during --dry-run push, the daemon closes
            // its socket early after receiving the file list since no actual data
            // transfer is needed.
            Ok(ClientSummary::default())
        }
        Err(e) => Err(invalid_argument_error(&format!("transfer failed: {e}"), 23)),
    }
}

/// Returns `true` if the I/O error indicates the remote side closed the connection.
///
/// During `--dry-run` push transfers, the upstream daemon closes its socket early
/// after processing the file list. This manifests as `BrokenPipe`, `ConnectionReset`,
/// or `UnexpectedEof` - all expected and should map to exit code 0.
///
/// upstream: clientserver.c - the server exits after file list processing when
/// `!do_xfers` (dry-run mode).
fn is_dry_run_remote_close(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::WouldBlock
    )
}

/// Converts server-side statistics to a client summary.
///
/// Maps statistics returned by the server (receiver or generator) into the
/// format expected by the client summary. The elapsed time is used to calculate
/// the transfer rate (bytes/sec) shown in the summary output.
fn convert_server_stats_to_summary(
    stats: crate::server::ServerStats,
    elapsed: Duration,
) -> ClientSummary {
    use crate::server::ServerStats;
    use engine::local_copy::LocalCopySummary;
    use transfer::io_error_flags;

    let (local_summary, io_error, error_count) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            let s = LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.bytes_received,
                transfer_stats.bytes_sent,
                transfer_stats.total_source_bytes,
                elapsed,
            );
            (s, transfer_stats.io_error, transfer_stats.error_count)
        }
        ServerStats::Generator(ref generator_stats) => {
            let s = LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.bytes_sent,
                elapsed,
            );
            (s, generator_stats.io_error, 0u32)
        }
    };

    let mut summary = ClientSummary::from_summary(local_summary);

    // upstream: log.c - log_exit() converts io_error bitfield to RERR_* codes.
    let exit_code = io_error_flags::to_exit_code(io_error);
    if exit_code != 0 {
        summary.set_io_error_exit_code(exit_code);
    } else if error_count > 0 {
        // Remote sender reported errors via MSG_ERROR - treat as partial transfer.
        summary.set_io_error_exit_code(23); // RERR_PARTIAL
    }

    summary
}

/// Runs server over a TCP stream with pre-negotiated handshake.
///
/// Used for daemon client mode where the protocol version exchange has already
/// been performed in `perform_daemon_handshake`. Calls `run_server_with_handshake`
/// directly, skipping the duplicate version exchange.
fn run_server_with_handshake_over_stream(
    config: ServerConfig,
    handshake: HandshakeResult,
    stream: &mut TcpStream,
    progress: Option<&mut dyn crate::server::TransferProgressCallback>,
) -> Result<crate::server::ServerStats, ClientError> {
    let mut reader = stream
        .try_clone()
        .map_err(|e| invalid_argument_error(&format!("failed to clone stream: {e}"), 23))?;

    crate::server::run_server_with_handshake(config, handshake, &mut reader, stream, progress)
        .map_err(|e| invalid_argument_error(&format!("transfer failed: {e}"), 23))
}

/// Builds server configuration for receiver role (pull transfer).
fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    server_config.connection.client_mode = true;
    server_config.connection.is_daemon_connection = true;
    server_config.connection.filter_rules = filter_rules;

    server_config.flags.verbose = config.verbosity() > 0;

    // upstream: numeric_ids and delete are --numeric-ids / --delete-* long-form args only.
    server_config.flags.numeric_ids = config.numeric_ids();
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();

    server_config.write.fsync = config.fsync();
    server_config.write.io_uring_policy = config.io_uring_policy();
    server_config.checksum_choice = config.checksum_protocol_override();
    server_config.connection.compression_level = config.compression_level();
    // upstream: options.c:2737-2740 - compress_level defaults to 6 when -z is set.
    if server_config.flags.compress && server_config.connection.compression_level.is_none() {
        server_config.connection.compression_level =
            Some(compress::zlib::CompressionLevel::Default);
    }
    server_config.stop_at = config.stop_at();
    server_config.reference_directories = config.reference_directories().to_vec();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    server_config.connection.client_mode = true;
    server_config.connection.is_daemon_connection = true;
    server_config.connection.filter_rules = filter_rules;

    server_config.flags.verbose = config.verbosity() > 0;

    // upstream: numeric_ids and delete are --numeric-ids / --delete-* long-form args only.
    server_config.flags.numeric_ids = config.numeric_ids();
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();

    server_config.write.fsync = config.fsync();
    server_config.write.io_uring_policy = config.io_uring_policy();
    server_config.checksum_choice = config.checksum_protocol_override();
    server_config.connection.compression_level = config.compression_level();
    // upstream: options.c:2737-2740 - compress_level defaults to 6 when -z is set.
    if server_config.flags.compress && server_config.connection.compression_level.is_none() {
        server_config.connection.compression_level =
            Some(compress::zlib::CompressionLevel::Default);
    }
    server_config.stop_at = config.stop_at();
    server_config.reference_directories = config.reference_directories().to_vec();

    // upstream: clientserver.c - when --files-from references a remote file
    // (colon prefix), the daemon receiver opens the file locally and forwards
    // its content to the client sender via start_filesfrom_forwarding().
    if config.files_from().is_remote() {
        server_config.file_selection.files_from_path = Some("-".to_owned());
        server_config.file_selection.from0 = true;
    }

    // upstream: options.c:2944 - when the client is the sender and --files-from
    // points to a local file, the sender reads the list directly.
    use super::super::super::config::FilesFromSource;
    match config.files_from() {
        FilesFromSource::LocalFile(path) => {
            server_config.file_selection.files_from_path = Some(path.to_string_lossy().to_string());
            server_config.file_selection.from0 = config.from0();
        }
        FilesFromSource::Stdin => {
            server_config.file_selection.files_from_path = Some("-".to_owned());
            server_config.file_selection.from0 = config.from0();
        }
        _ => {}
    }

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

#[cfg(test)]
mod tests {
    use super::super::connection::DaemonTransferRequest;
    use super::*;
    use crate::client::module_list::DaemonAddress;

    mod protect_args_daemon_tests {
        use super::*;

        fn test_daemon_request() -> DaemonTransferRequest {
            DaemonTransferRequest {
                address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
                module: "test".to_owned(),
                path: String::new(),
                username: None,
            }
        }

        #[test]
        fn build_minimal_args_receiver() {
            let args = build_minimal_daemon_args(false);
            assert_eq!(args, vec!["--server", "-s", "."]);
        }

        #[test]
        fn build_minimal_args_sender() {
            let args = build_minimal_daemon_args(true);
            assert_eq!(args, vec!["--server", "--sender", "-s", "."]);
        }

        #[test]
        fn build_full_args_includes_module_path() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert_eq!(args[0], "--server");
            assert!(args.contains(&".".to_owned()));
            let last = args.last().unwrap();
            assert!(last.starts_with(&request.module));
        }

        #[test]
        fn build_full_args_sender_flag() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, true);

            assert_eq!(args[0], "--server");
            assert_eq!(args[1], "--sender");
        }

        #[test]
        fn build_full_args_capability_flags_protocol30() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(args.iter().any(|a| a.starts_with("-e.")));
        }

        #[test]
        fn build_full_args_no_capability_flags_protocol29() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(29u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(!args.iter().any(|a| a.starts_with("-e.")));
        }

        #[test]
        fn build_full_args_includes_compare_dest() {
            let config = ClientConfig::builder()
                .compare_destination("/tmp/compare")
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                args.iter().any(|a| a == "--compare-dest=/tmp/compare"),
                "expected --compare-dest=/tmp/compare in args: {args:?}"
            );
        }

        #[test]
        fn build_full_args_includes_copy_dest() {
            let config = ClientConfig::builder()
                .copy_destination("/tmp/copy")
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                args.iter().any(|a| a == "--copy-dest=/tmp/copy"),
                "expected --copy-dest=/tmp/copy in args: {args:?}"
            );
        }

        #[test]
        fn build_full_args_includes_link_dest() {
            let config = ClientConfig::builder().link_destination("/prev").build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                args.iter().any(|a| a == "--link-dest=/prev"),
                "expected --link-dest=/prev in args: {args:?}"
            );
        }

        #[test]
        fn build_full_args_includes_multiple_reference_dirs() {
            let config = ClientConfig::builder()
                .link_destination("/prev1")
                .link_destination("/prev2")
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                args.iter().any(|a| a == "--link-dest=/prev1"),
                "expected --link-dest=/prev1 in args: {args:?}"
            );
            assert!(
                args.iter().any(|a| a == "--link-dest=/prev2"),
                "expected --link-dest=/prev2 in args: {args:?}"
            );
        }

        #[test]
        fn build_full_args_omits_reference_dirs_when_empty() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                !args.iter().any(|a| a.starts_with("--compare-dest=")
                    || a.starts_with("--copy-dest=")
                    || a.starts_with("--link-dest=")),
                "should not emit reference dir args when empty: {args:?}"
            );
        }

        #[test]
        fn build_full_args_omits_reference_dirs_in_pull_mode() {
            // upstream: options.c:2915-2923 - reference dirs are inside if(am_sender).
            let config = ClientConfig::builder()
                .compare_destination("/tmp/compare")
                .link_destination("/prev")
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, true);

            assert!(
                !args.iter().any(|a| a.starts_with("--compare-dest=")
                    || a.starts_with("--copy-dest=")
                    || a.starts_with("--link-dest=")),
                "pull mode should not send reference dir args to daemon: {args:?}"
            );
        }
    }

    mod server_config_reference_dirs {
        use super::*;
        use crate::client::config::ReferenceDirectoryKind;

        #[test]
        fn receiver_config_propagates_reference_directories() {
            let config = ClientConfig::builder()
                .compare_destination("/tmp/compare")
                .link_destination("/prev")
                .build();
            let server_config =
                build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new())
                    .unwrap();

            assert_eq!(server_config.reference_directories.len(), 2);
            assert_eq!(
                server_config.reference_directories[0].kind(),
                ReferenceDirectoryKind::Compare
            );
            assert_eq!(
                server_config.reference_directories[0]
                    .path()
                    .to_str()
                    .unwrap(),
                "/tmp/compare"
            );
            assert_eq!(
                server_config.reference_directories[1].kind(),
                ReferenceDirectoryKind::Link
            );
            assert_eq!(
                server_config.reference_directories[1]
                    .path()
                    .to_str()
                    .unwrap(),
                "/prev"
            );
        }

        #[test]
        fn generator_config_propagates_reference_directories() {
            let config = ClientConfig::builder()
                .copy_destination("/tmp/copy")
                .build();
            let server_config =
                build_server_config_for_generator(&config, &["src".to_owned()], Vec::new())
                    .unwrap();

            assert_eq!(server_config.reference_directories.len(), 1);
            assert_eq!(
                server_config.reference_directories[0].kind(),
                ReferenceDirectoryKind::Copy
            );
            assert_eq!(
                server_config.reference_directories[0]
                    .path()
                    .to_str()
                    .unwrap(),
                "/tmp/copy"
            );
        }

        #[test]
        fn receiver_config_empty_reference_dirs_by_default() {
            let config = ClientConfig::default();
            let server_config =
                build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new())
                    .unwrap();

            assert!(server_config.reference_directories.is_empty());
        }

        #[test]
        fn generator_config_empty_reference_dirs_by_default() {
            let config = ClientConfig::default();
            let server_config =
                build_server_config_for_generator(&config, &["src".to_owned()], Vec::new())
                    .unwrap();

            assert!(server_config.reference_directories.is_empty());
        }

        #[test]
        fn generator_config_sets_files_from_for_local_file_push() {
            // upstream: options.c:2944 - when the client is the sender and
            // --files-from points to a local file, the generator reads filenames
            // directly from the file (not via the protocol stream).
            let config = ClientConfig::builder()
                .files_from(crate::client::config::FilesFromSource::LocalFile(
                    std::path::PathBuf::from("/tmp/list.txt"),
                ))
                .build();

            let local_paths = vec!["src/".to_owned()];
            let server_config =
                build_server_config_for_generator(&config, &local_paths, Vec::new()).unwrap();

            assert_eq!(
                server_config.file_selection.files_from_path.as_deref(),
                Some("/tmp/list.txt"),
                "generator should read files-from from local file for push"
            );
        }

        #[test]
        fn generator_config_sets_files_from_for_remote_source() {
            let config = ClientConfig::builder()
                .files_from(crate::client::config::FilesFromSource::RemoteFile(
                    "/srv/list.txt".to_owned(),
                ))
                .build();

            let local_paths = vec!["src/".to_owned()];
            let server_config =
                build_server_config_for_generator(&config, &local_paths, Vec::new()).unwrap();

            assert_eq!(
                server_config.file_selection.files_from_path.as_deref(),
                Some("-"),
                "generator should read files-from from protocol stream for remote source"
            );
            assert!(
                server_config.file_selection.from0,
                "remote files-from uses NUL-separated wire format"
            );
        }
    }

    mod dry_run_remote_close_tests {
        use super::*;
        use std::io;

        #[test]
        fn broken_pipe_is_remote_close() {
            let err = io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe");
            assert!(is_dry_run_remote_close(&err));
        }

        #[test]
        fn connection_reset_is_remote_close() {
            let err = io::Error::new(io::ErrorKind::ConnectionReset, "connection reset");
            assert!(is_dry_run_remote_close(&err));
        }

        #[test]
        fn connection_aborted_is_remote_close() {
            let err = io::Error::new(io::ErrorKind::ConnectionAborted, "connection aborted");
            assert!(is_dry_run_remote_close(&err));
        }

        #[test]
        fn unexpected_eof_is_remote_close() {
            let err = io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected eof");
            assert!(is_dry_run_remote_close(&err));
        }

        #[test]
        fn timeout_is_not_remote_close() {
            let err = io::Error::new(io::ErrorKind::TimedOut, "timed out");
            assert!(!is_dry_run_remote_close(&err));
        }

        #[test]
        fn permission_denied_is_not_remote_close() {
            let err = io::Error::new(io::ErrorKind::PermissionDenied, "permission denied");
            assert!(!is_dry_run_remote_close(&err));
        }

        #[test]
        fn connection_refused_is_not_remote_close() {
            let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
            assert!(!is_dry_run_remote_close(&err));
        }

        #[test]
        fn other_error_is_not_remote_close() {
            let err = io::Error::other("some other error");
            assert!(!is_dry_run_remote_close(&err));
        }
    }

    mod files_from_daemon_args_tests {
        use super::*;
        use crate::client::config::FilesFromSource;
        use std::path::PathBuf;

        fn test_daemon_request() -> DaemonTransferRequest {
            DaemonTransferRequest {
                address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
                module: "test".to_owned(),
                path: String::new(),
                username: None,
            }
        }

        #[test]
        fn push_with_local_file_omits_files_from_arg() {
            // upstream: options.c:2944 - when client is sender and files_from
            // is local, the arg is NOT sent to the daemon.
            let config = ClientConfig::builder()
                .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();

            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                !args.iter().any(|a| a.starts_with("--files-from")),
                "push should not send --files-from to daemon: {args:?}"
            );
            assert!(
                !args.iter().any(|a| a == "--from0"),
                "push should not send --from0 to daemon: {args:?}"
            );
        }

        #[test]
        fn push_with_stdin_omits_files_from_arg() {
            let config = ClientConfig::builder()
                .files_from(FilesFromSource::Stdin)
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();

            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                !args.iter().any(|a| a.starts_with("--files-from")),
                "push with stdin should not send --files-from to daemon: {args:?}"
            );
        }

        #[test]
        fn pull_with_local_file_sends_files_from_stdin() {
            // upstream: options.c:2944 - when client is receiver (pull), local
            // files are forwarded as --files-from=- with --from0.
            let config = ClientConfig::builder()
                .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();

            let args = build_full_daemon_args(&config, &request, protocol, true);

            assert!(
                args.iter().any(|a| a == "--files-from=-"),
                "pull should send --files-from=- to daemon: {args:?}"
            );
            assert!(
                args.iter().any(|a| a == "--from0"),
                "pull should send --from0 to daemon: {args:?}"
            );
        }

        #[test]
        fn pull_with_stdin_sends_files_from_stdin() {
            let config = ClientConfig::builder()
                .files_from(FilesFromSource::Stdin)
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();

            let args = build_full_daemon_args(&config, &request, protocol, true);

            assert!(
                args.iter().any(|a| a == "--files-from=-"),
                "pull with stdin should send --files-from=- to daemon: {args:?}"
            );
        }

        #[test]
        fn push_with_remote_file_sends_files_from_path() {
            let config = ClientConfig::builder()
                .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();

            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                args.iter().any(|a| a == "--files-from=/remote/list.txt"),
                "should send remote --files-from path: {args:?}"
            );
        }

        #[test]
        fn pull_with_remote_file_sends_files_from_path() {
            let config = ClientConfig::builder()
                .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
                .build();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();

            let args = build_full_daemon_args(&config, &request, protocol, true);

            assert!(
                args.iter().any(|a| a == "--files-from=/remote/list.txt"),
                "should send remote --files-from path: {args:?}"
            );
        }

        #[test]
        fn no_files_from_omits_arg() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();

            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(
                !args.iter().any(|a| a.starts_with("--files-from")),
                "should not include --files-from: {args:?}"
            );
        }
    }

    mod files_from_forwarding_tests {
        use super::*;
        use crate::client::config::{ClientConfigBuilder, FilesFromSource};
        use std::io::Cursor;

        fn test_builder() -> ClientConfigBuilder {
            ClientConfigBuilder::default().transfer_args(["/src", "rsync://host/mod/"])
        }

        #[test]
        fn read_from_local_file_newline_delimited() {
            let dir = tempfile::tempdir().unwrap();
            let list_file = dir.path().join("list.txt");
            std::fs::write(&list_file, "file1.txt\nfile2.txt\nsubdir/file3.txt\n").unwrap();

            let config = test_builder()
                .files_from(FilesFromSource::LocalFile(list_file))
                .build();

            let data = read_files_from_for_forwarding(&config).unwrap();

            let mut reader = Cursor::new(&data);
            let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
            assert_eq!(
                filenames,
                vec!["file1.txt", "file2.txt", "subdir/file3.txt"]
            );
        }

        #[test]
        fn read_from_local_file_nul_delimited() {
            let dir = tempfile::tempdir().unwrap();
            let list_file = dir.path().join("list.txt");
            std::fs::write(&list_file, "alpha.txt\0beta.txt\0").unwrap();

            let config = test_builder()
                .files_from(FilesFromSource::LocalFile(list_file))
                .from0(true)
                .build();

            let data = read_files_from_for_forwarding(&config).unwrap();

            let mut reader = Cursor::new(&data);
            let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
            assert_eq!(filenames, vec!["alpha.txt", "beta.txt"]);
        }

        #[test]
        fn read_from_nonexistent_file_returns_error() {
            let config = test_builder()
                .files_from(FilesFromSource::LocalFile(std::path::PathBuf::from(
                    "/nonexistent/list.txt",
                )))
                .build();

            let err = read_files_from_for_forwarding(&config).unwrap_err();
            assert!(err.to_string().contains("failed to open --files-from"));
        }

        #[test]
        fn no_forwarding_for_none() {
            let config = test_builder().build();

            let data = read_files_from_for_forwarding(&config).unwrap();
            assert!(data.is_empty());
        }

        #[test]
        fn no_forwarding_for_remote_file() {
            let config = test_builder()
                .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
                .build();

            let data = read_files_from_for_forwarding(&config).unwrap();
            assert!(data.is_empty());
        }

        #[test]
        fn empty_local_file_produces_terminator() {
            let dir = tempfile::tempdir().unwrap();
            let list_file = dir.path().join("empty.txt");
            std::fs::write(&list_file, "").unwrap();

            let config = test_builder()
                .files_from(FilesFromSource::LocalFile(list_file))
                .build();

            let data = read_files_from_for_forwarding(&config).unwrap();

            assert_eq!(data, b"\0\0");

            let mut reader = Cursor::new(&data);
            let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
            assert!(filenames.is_empty());
        }

        #[test]
        fn roundtrip_with_crlf_line_endings() {
            let dir = tempfile::tempdir().unwrap();
            let list_file = dir.path().join("list.txt");
            std::fs::write(&list_file, "file1.txt\r\nfile2.txt\r\n").unwrap();

            let config = test_builder()
                .files_from(FilesFromSource::LocalFile(list_file))
                .build();

            let data = read_files_from_for_forwarding(&config).unwrap();

            let mut reader = Cursor::new(&data);
            let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
            assert_eq!(filenames, vec!["file1.txt", "file2.txt"]);
        }

        #[test]
        fn files_from_data_on_connection_config() {
            use transfer::config::ConnectionConfig;

            let mut conn = ConnectionConfig::default();
            assert!(conn.files_from_data.is_none());

            conn.files_from_data = Some(b"file1.txt\0file2.txt\0\0".to_vec());
            assert!(conn.files_from_data.is_some());

            let data = conn.files_from_data.take().unwrap();
            assert_eq!(data, b"file1.txt\0file2.txt\0\0");
            assert!(conn.files_from_data.is_none());
        }
    }
}
