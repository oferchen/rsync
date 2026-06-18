//! SSH transfer orchestration.
//!
//! This module coordinates SSH-based remote transfers by spawning SSH connections,
//! negotiating the rsync protocol, and executing transfers using the server
//! infrastructure. It mirrors the flow in upstream `main.c:do_cmd()` where the
//! client forks the remote shell, sets up pipes, and dispatches to the sender
//! or receiver role.
//!
//! # Architecture
//!
//! Transfers use the `SshConnection::split` method to obtain separate read/write
//! halves, which are then passed to the server infrastructure for protocol handling.
//!
//! # Upstream Reference
//!
//! - `main.c:do_cmd()` - SSH fork/exec and pipe setup
//! - `main.c:client_run()` - Role dispatch after SSH connection
//! - `options.c:server_options()` - Remote `--server` argument construction

use std::ffi::{OsStr, OsString};
use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "tracing")]
use tracing::instrument;

use engine::batch::BatchWriter;
use rsync_io::ssh::{SshCommand, SshConnection, parse_ssh_operand};

use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error, invalid_argument_error_typed};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::files_from_forwarding::read_local_files_from_for_forwarding;
use super::flags;
use super::invocation::{
    RemoteInvocationBuilder, RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role,
};
use crate::exit_code::ExitCode;
use crate::server::{ServerConfig, ServerRole, TransferProgressCallback, TransferProgressEvent};

/// Adapts a [`ClientProgressObserver`] to [`TransferProgressCallback`].
///
/// Converts server-side per-file progress events into client-side progress
/// updates, enabling live progress display during SSH and daemon transfers.
struct ServerProgressAdapter<'a> {
    observer: &'a mut dyn ClientProgressObserver,
    start: Instant,
    overall_transferred: u64,
}

impl<'a> ServerProgressAdapter<'a> {
    fn new(observer: &'a mut dyn ClientProgressObserver, start: Instant) -> Self {
        Self {
            observer,
            start,
            overall_transferred: 0,
        }
    }
}

impl TransferProgressCallback for ServerProgressAdapter<'_> {
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>) {
        use std::path::Path;
        use std::sync::Arc;

        self.overall_transferred += event.file_bytes;

        let client_event = super::super::summary::ClientEvent::from_progress(
            event.path,
            event.file_bytes,
            event.total_file_bytes,
            self.start.elapsed(),
            Arc::from(Path::new("")),
        );

        let update = super::super::progress::ClientProgressUpdate::from_transfer_event(
            client_event,
            event.files_done,
            event.total_files,
            event.total_file_bytes,
            self.overall_transferred,
            self.start.elapsed(),
            event.flist_eof,
        );

        self.observer.on_progress(&update);
    }
}

/// SSH invocation result containing args, host, optional user, optional port, and stdin args.
///
/// Used by `parse_single_remote` and `parse_remote_operands` to return parsed
/// remote connection information along with the rsync invocation arguments.
/// The final `Vec<String>` contains arguments to send over stdin when
/// secluded-args is active (empty when disabled).
type SshInvocationResult = (
    Vec<OsString>,
    String,
    Option<String>,
    Option<u16>,
    Vec<String>,
);

/// Executes a transfer over SSH transport.
///
/// This is the main entry point for SSH-based remote transfers, mirroring
/// upstream `main.c:do_cmd()`. It:
/// 1. Determines push vs pull from operand positions
/// 2. Parses the remote operand
/// 3. Builds the remote rsync invocation (upstream: `options.c:server_options()`)
/// 4. Spawns an SSH connection (upstream: `main.c:do_cmd()`)
/// 5. Negotiates the protocol
/// 6. Executes the transfer using server infrastructure
///
/// # Arguments
///
/// * `config` - Client configuration with transfer options
/// * `observer` - Optional progress observer
///
/// # Returns
///
/// A summary of the transfer on success, or an error if any step fails.
///
/// # Errors
///
/// Returns error if:
/// - Remote operand parsing fails
/// - SSH connection fails
/// - Protocol negotiation fails
/// - Transfer execution fails
#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "ssh_transfer")
)]
pub fn run_ssh_transfer(
    config: &ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    let args = config.transfer_args();
    if args.len() < 2 {
        return Err(invalid_argument_error(
            "need at least one source and one destination",
            1,
        ));
    }

    let (sources, destination) = args.split_at(args.len() - 1);
    let destination = &destination[0];

    let transfer_spec = determine_transfer_role(sources, destination)?;

    match transfer_spec {
        TransferSpec::Push {
            local_sources,
            remote_dest,
        } => {
            let (invocation_args, ssh_host, ssh_user, ssh_port, stdin_args) =
                parse_single_remote(&remote_dest, config, RemoteRole::Sender)?;
            let connection = build_ssh_connection(
                &ssh_user,
                &ssh_host,
                ssh_port,
                &invocation_args,
                config,
                &stdin_args,
            )?;
            run_push_transfer(config, connection, &local_sources, observer, batch_writer)
        }
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            let (invocation_args, ssh_host, ssh_user, ssh_port, stdin_args) =
                parse_remote_operands(&remote_sources, config, RemoteRole::Receiver)?;
            let connection = build_ssh_connection(
                &ssh_user,
                &ssh_host,
                ssh_port,
                &invocation_args,
                config,
                &stdin_args,
            )?;
            run_pull_transfer(config, connection, &[local_dest], observer, batch_writer)
        }
        TransferSpec::Proxy {
            remote_sources,
            remote_dest,
        } => run_proxy_transfer(config, remote_sources, remote_dest, observer),
    }
}

/// Parses a single remote operand and builds the invocation args.
pub(super) fn parse_single_remote(
    operand_str: &str,
    config: &ClientConfig,
    role: RemoteRole,
) -> Result<SshInvocationResult, ClientError> {
    let operand = parse_ssh_operand(OsStr::new(operand_str))
        .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

    let invocation_builder = RemoteInvocationBuilder::new(config, role);
    let secluded = invocation_builder.build_secluded(&[operand.path()]);

    Ok((
        secluded.command_line_args,
        operand.host().to_owned(),
        operand.user().map(String::from),
        operand.port(),
        secluded.stdin_args,
    ))
}

/// Parses remote operand(s) and builds the invocation args.
pub(super) fn parse_remote_operands(
    remote_operands: &RemoteOperands,
    config: &ClientConfig,
    role: RemoteRole,
) -> Result<SshInvocationResult, ClientError> {
    match remote_operands {
        RemoteOperands::Single(operand_str) => parse_single_remote(operand_str, config, role),
        RemoteOperands::Multiple(operand_strs) => {
            let first_operand = parse_ssh_operand(OsStr::new(&operand_strs[0]))
                .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

            let mut paths = Vec::new();
            for operand_str in operand_strs {
                let operand = parse_ssh_operand(OsStr::new(operand_str)).map_err(|e| {
                    invalid_argument_error(&format!("invalid remote operand: {e}"), 1)
                })?;
                paths.push(operand.path().to_owned());
            }

            let invocation_builder = RemoteInvocationBuilder::new(config, role);
            let path_refs: Vec<&str> = paths.iter().map(|s| s.as_ref()).collect();
            let secluded = invocation_builder.build_secluded(&path_refs);

            Ok((
                secluded.command_line_args,
                first_operand.host().to_owned(),
                first_operand.user().map(String::from),
                first_operand.port(),
                secluded.stdin_args,
            ))
        }
    }
}

/// Builds and spawns an SSH connection with the remote rsync invocation.
///
/// When `stdin_args` is non-empty (secluded-args mode), the arguments are
/// sent over stdin immediately after spawning the SSH process, before
/// returning the connection for protocol negotiation.
fn build_ssh_connection(
    user: &Option<String>,
    host: &str,
    port: Option<u16>,
    invocation_args: &[OsString],
    config: &ClientConfig,
    stdin_args: &[String],
) -> Result<SshConnection, ClientError> {
    let mut ssh = SshCommand::new(host);

    if let Some(user) = user {
        ssh.set_user(user);
    }

    if let Some(port) = port {
        ssh.set_port(port);
    }

    if let Some(shell_args) = config.remote_shell()
        && !shell_args.is_empty()
    {
        ssh.set_program(&shell_args[0]);
        for arg in &shell_args[1..] {
            ssh.push_option(arg.clone());
        }
    }

    // upstream: clientserver.c start_socket_client() binds the local address;
    // forward --address to SSH as -o BindAddress=<addr>.
    if let Some(bind_addr) = config.bind_address() {
        ssh.set_bind_address(Some(bind_addr.socket().ip()));
    }

    ssh.set_prefer_aes_gcm(config.prefer_aes_gcm());
    ssh.set_jump_hosts(config.jump_hosts().map(OsString::from));

    // upstream: options.c - contimeout is forwarded as SSH's -o ConnectTimeout.
    let connect_timeout = config.connect_timeout().effective(Duration::from_secs(30));
    ssh.set_connect_timeout(connect_timeout);

    ssh.set_remote_command(invocation_args);

    warn_double_compression_once(config.compress(), ssh.has_ssh_compression());

    // upstream: pipe.c:85 - SSH spawn failures return IPC error code.
    let mut connection = ssh.spawn().map_err(|e| {
        invalid_argument_error(
            &format!("failed to spawn SSH connection: {e}"),
            super::super::IPC_EXIT_CODE,
        )
    })?;

    // upstream: rsync.c:283-320 send_protected_args() sends args as
    // null-separated strings over the pipe before protocol negotiation begins,
    // applying iconvbufs(ic_send, ...) to each arg when iconv is configured
    // (compat.c:799-806 filesfrom_convert / protect-args iconv gating).
    if !stdin_args.is_empty() {
        let arg_refs: Vec<&str> = stdin_args.iter().map(String::as_str).collect();
        // upstream: rsync.c:296-297 - DEBUG_GTE(CMD, 1) emits
        // `print_child_argv("protected args:", args + i + 1)` right before the
        // per-arg `iconvbufs(ic_send, ...)` loop. `arg_refs` is the same
        // payload we are about to ship over stdin.
        protocol::cmd::trace_protected_args(&arg_refs);
        let iconv_converter = if config.protect_args().unwrap_or(false) {
            config.iconv().resolve_converter()
        } else {
            None
        };
        protocol::secluded_args::send_secluded_args(
            &mut connection,
            &arg_refs,
            iconv_converter.as_ref(),
        )
        .map_err(|e| {
            invalid_argument_error(
                &format!("failed to send secluded args: {e}"),
                super::super::IPC_EXIT_CODE,
            )
        })?;
    }

    Ok(connection)
}

/// Executes a pull transfer (remote → local).
///
/// In a pull transfer, the local side acts as the receiver and the remote side
/// acts as the sender/generator. We reuse the server receiver infrastructure.
fn run_pull_transfer(
    config: &ClientConfig,
    connection: SshConnection,
    local_paths: &[String],
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    // upstream: main.c:1258 - client_mode=true tells the server flow to send the
    // filter list after handshake + compat exchange (where recv_filter_list() is
    // called inside the server).
    let mut server_config = build_server_config_for_receiver(config, local_paths)?;
    server_config.connection.client_mode = true;
    server_config.connection.filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded()).map_err(
            |e| invalid_argument_error(&format!("failed to build filter rules: {e}"), 12),
        )?;
    server_config.stop_at = config.stop_at();

    // upstream: main.c:1372-1375 - when pulling with --files-from pointing to a
    // local file or stdin, the receiver reads the file list locally and
    // forwards its bytes back to the remote sender via
    // `start_filesfrom_forwarding(filesfrom_fd)`. The remote sender consumes
    // the forwarded bytes through its protocol stream.
    if config.files_from().is_local_forwarded() {
        let data = read_local_files_from_for_forwarding(config)?;
        server_config.connection.files_from_data = Some(data);
    }

    let batch_ctx = batch_writer.map(|bw| build_batch_context(config, bw));

    let start = Instant::now();
    let mut adapter = observer.map(|obs| ServerProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);
    let server_stats =
        run_server_over_ssh_connection(server_config, connection, progress, batch_ctx)?;
    let elapsed = start.elapsed();

    Ok(convert_server_stats_to_summary(server_stats, elapsed))
}

/// Executes a push transfer (local → remote).
///
/// In a push transfer, the local side acts as the sender/generator and the
/// remote side acts as the receiver. We reuse the server generator infrastructure.
fn run_push_transfer(
    config: &ClientConfig,
    connection: SshConnection,
    local_paths: &[String],
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    // upstream: client_mode=true ensures the filter list is sent after
    // handshake + compat exchange.
    let mut server_config = build_server_config_for_generator(config, local_paths)?;
    server_config.connection.client_mode = true;
    server_config.connection.filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded()).map_err(
            |e| invalid_argument_error(&format!("failed to build filter rules: {e}"), 12),
        )?;
    server_config.stop_at = config.stop_at();

    let batch_ctx = batch_writer.map(|bw| build_batch_context(config, bw));

    let start = Instant::now();
    let mut adapter = observer.map(|obs| ServerProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);
    let server_stats =
        run_server_over_ssh_connection(server_config, connection, progress, batch_ctx)?;
    let elapsed = start.elapsed();

    Ok(convert_server_stats_to_summary(server_stats, elapsed))
}

/// Executes a proxy transfer (remote → remote via local).
///
/// In a proxy transfer, the local machine relays protocol messages between
/// two remote hosts. We spawn two SSH connections:
/// 1. To the source with `rsync --server --sender` (acts as generator)
/// 2. To the destination with `rsync --server` (acts as receiver)
///
/// Data flows: source → local (relay) → destination
fn run_proxy_transfer(
    config: &ClientConfig,
    remote_sources: RemoteOperands,
    remote_dest: String,
    _observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    use super::remote_to_remote::run_remote_to_remote_transfer;

    run_remote_to_remote_transfer(config, remote_sources, remote_dest)
}

/// Converts server-side statistics to a client summary.
///
/// Maps the statistics returned by the server (receiver or generator) into the
/// format expected by the client summary. Uses the available server statistics
/// (files listed, files transferred, and bytes sent/received) to create a
/// LocalCopySummary with the most relevant fields populated. The elapsed time
/// is used to calculate the transfer rate (bytes/sec) shown in the summary output.
pub(super) fn convert_server_stats_to_summary(
    stats: crate::server::ServerStats,
    elapsed: Duration,
) -> ClientSummary {
    use crate::server::ServerStats;
    use engine::local_copy::LocalCopySummary;
    use transfer::io_error_flags;

    let (local_summary, io_error, error_count) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            // SSH-pull: local side ran the receiver and its `--delete` sweep.
            let s = LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.bytes_received,
                transfer_stats.bytes_sent,
                transfer_stats.total_source_bytes,
                elapsed,
                transfer_stats.literal_data,
                transfer_stats.matched_data,
                u64::from(transfer_stats.delete_stats.total()),
            );
            (s, transfer_stats.io_error, transfer_stats.error_count)
        }
        ServerStats::Generator(ref generator_stats) => {
            // SSH-push: local side ran the sender/generator; the remote
            // receiver reported its delete counters via `NDX_DEL_STATS`.
            let s = LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.bytes_sent,
                elapsed,
                u64::from(generator_stats.delete_stats.total()),
            );
            (s, generator_stats.io_error, 0u32)
        }
    };

    let mut summary = ClientSummary::from_summary(local_summary);

    // upstream: log.c log_exit() - convert io_error bitfield to RERR_* codes.
    let exit_code = io_error_flags::to_exit_code(io_error);
    if exit_code != 0 {
        summary.set_io_error_exit_code(exit_code);
    } else if error_count > 0 {
        // Remote sender reported errors via MSG_ERROR - treat as RERR_PARTIAL.
        summary.set_io_error_exit_code(23);
    }

    summary
}

/// Maps an SSH child process exit status to an rsync exit code.
///
/// Mirrors upstream rsync's `wait_process_with_flush()` logic in `main.c`:
/// - Exit 0: success
/// - Exit 127: command not found (`RERR_CMD_NOTFOUND`)
/// - Exit 255: SSH connection failure (`RERR_CMD_FAILED`)
/// - Killed by signal: `RERR_CMD_KILLED`
/// - Other rsync exit codes: passed through directly
/// - Unknown codes: fall back to `PartialTransfer`
pub(super) fn map_child_exit_status(status: std::process::ExitStatus) -> ExitCode {
    if status.success() {
        return ExitCode::Ok;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal().is_some() {
            return ExitCode::CommandKilled;
        }
    }

    match status.code() {
        // upstream: main.c:1591 - shell exit codes mapped to RERR_CMD_*
        Some(126) => ExitCode::CommandRun,
        Some(127) => ExitCode::CommandNotFound,
        Some(255) => ExitCode::CommandFailed,
        Some(code) => ExitCode::from_i32(code).unwrap_or(ExitCode::PartialTransfer),
        None => ExitCode::WaitChild,
    }
}

/// Formats captured SSH stderr output as a suffix for error messages.
///
/// Returns an empty string when `stderr_bytes` is empty. Otherwise returns
/// a newline-separated block prefixed with "SSH stderr:" that gives the user
/// visibility into what the remote process wrote to stderr before exiting.
/// The output is trimmed to remove trailing whitespace.
pub(super) fn format_stderr_context(stderr_bytes: &[u8]) -> String {
    if stderr_bytes.is_empty() {
        return String::new();
    }
    let text = String::from_utf8_lossy(stderr_bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!("\nSSH stderr:\n{trimmed}")
}

/// Batch recording context passed through the SSH transfer chain.
use super::batch_support::{BatchContext, build_batch_context, build_batch_recording};

/// Runs server over an SSH connection using split read/write halves.
///
/// This uses `SshConnection::split` to obtain separate reader and writer handles,
/// avoiding the need for unsafe aliased mutable references.
///
/// When `batch_writer` is provided, the handshake is performed first, then the
/// batch header is written with negotiated protocol info, and the appropriate
/// I/O side is wrapped with a tee to record protocol bytes to the batch file.
///
/// upstream: `io.c:start_write_batch()` activates the tee after handshake,
/// recording either incoming (receiver) or outgoing (sender) protocol data.
///
/// After the transfer completes, the SSH child process is waited on and its exit
/// status is mapped to an rsync exit code. The worst (highest) exit code from the
/// transfer result and the child exit status is propagated, mirroring upstream
/// rsync's `wait_process_with_flush()` behavior.
fn run_server_over_ssh_connection(
    config: ServerConfig,
    connection: SshConnection,
    progress: Option<&mut dyn crate::server::TransferProgressCallback>,
    batch_ctx: Option<BatchContext>,
) -> Result<crate::server::ServerStats, ClientError> {
    let (reader, mut writer, mut child_handle) = connection
        .split()
        .map_err(|e| invalid_argument_error(&format!("failed to split SSH connection: {e}"), 23))?;

    // upstream: io.c read_buf() uses 32KB read-ahead buffering. Without this,
    // each multiplex frame header (4 bytes) + payload triggers separate syscalls
    // on the SSH pipe.
    let mut reader = BufReader::with_capacity(32768, reader);

    let batch_recording = batch_ctx.as_ref().map(|ctx| {
        let is_sender = config.role == ServerRole::Generator;
        build_batch_recording(ctx, is_sender)
    });

    let handshake = match crate::server::perform_handshake(&mut reader, &mut writer) {
        Ok(h) => h,
        Err(e) => {
            // Capture SSH stderr - the remote process likely wrote diagnostic
            // output (e.g., "Connection refused") that explains the failure.
            drop(writer);
            let stderr_text = match child_handle.wait_with_stderr() {
                Ok((_, stderr_bytes)) => format_stderr_context(&stderr_bytes),
                Err(_) => String::new(),
            };
            return Err(invalid_argument_error(
                &format!("handshake failed: {e}{stderr_text}"),
                5,
            ));
        }
    };

    // upstream: --contimeout - if watchdog already fired (timeout expired during
    // handshake), map to exit code 35 (RERR_CONTIMEOUT).
    if let Err(e) = child_handle.cancel_connect_watchdog() {
        return Err(invalid_argument_error(
            &format!("{e}"),
            crate::exit_code::ExitCode::ConnectionTimeout.as_i32(),
        ));
    }
    let transfer_result = crate::server::run_server_with_handshake(
        config,
        handshake,
        &mut reader,
        &mut writer,
        progress,
        batch_recording,
        None,
    );

    // Close the writer to signal EOF so the remote process can exit.
    drop(writer);

    // upstream: main.c wait_process_with_flush() - wait for child and map status.
    let (child_exit_code, stderr_text) = match child_handle.wait_with_stderr() {
        Ok((status, stderr_bytes)) => {
            let stderr_text = format_stderr_context(&stderr_bytes);
            (map_child_exit_status(status), stderr_text)
        }
        Err(_) => (ExitCode::WaitChild, String::new()),
    };

    match transfer_result {
        Ok(stats) => {
            // upstream: take MAX of transfer and child exit codes.
            if child_exit_code.is_success() {
                Ok(stats)
            } else {
                Err(invalid_argument_error_typed(
                    &format!(
                        "remote process exited with error: {}{stderr_text}",
                        child_exit_code.description()
                    ),
                    child_exit_code,
                ))
            }
        }
        Err(transfer_error) => {
            let transfer_exit = ExitCode::from_io_error(&transfer_error);
            if child_exit_code.as_i32() > transfer_exit.as_i32() {
                Err(invalid_argument_error_typed(
                    &format!(
                        "transfer failed and remote process exited with error: {}{stderr_text}",
                        child_exit_code.description()
                    ),
                    child_exit_code,
                ))
            } else {
                Err(invalid_argument_error(
                    &format!("transfer failed: {transfer_error}{stderr_text}"),
                    transfer_exit.as_i32(),
                ))
            }
        }
    }
}

/// Builds server configuration for receiver role (pull transfer).
pub(super) fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Propagate long-form-only flags that aren't part of the compact flag string.
    // upstream: numeric_ids and delete are --numeric-ids / --delete-* long-form args only.
    server_config.flags.numeric_ids = config.numeric_ids();
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
///
/// Propagates `--files-from` plumbing for the local sender (generator) so the
/// file list is built from the requested entry list rather than the source
/// directory's full tree walk.
///
/// # Upstream Reference
///
/// - `options.c:2465-2510` - the sender opens a local files-from file (or
///   sets up filesfrom_fd for remote/stdin sources).
/// - `flist.c:2275-2298` - `send_file_list()` chdirs to `argv[0]` then reads
///   filenames from `filesfrom_fd` to emit the file list.
/// - `main.c:1322-1328` - when `filesfrom_host` is non-NULL, the sender
///   wires `filesfrom_fd = f_in` so the remote forwards bytes via the wire.
pub(super) fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Propagate long-form-only flags that aren't part of the compact flag string.
    // upstream: numeric_ids and delete are --numeric-ids / --delete-* long-form args only.
    server_config.flags.numeric_ids = config.numeric_ids();
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();

    apply_files_from_for_sender(config, &mut server_config);

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Wires `--files-from` into a sender (`Generator`) server configuration.
///
/// The local sender resolves entries relative to `argv[0]` (the first transfer
/// operand) and emits a file list constrained to those entries instead of
/// walking the entire source tree. Without this wiring the generator would
/// recurse the absolute source directory and (under `--relative`, implied by
/// `--files-from`) mirror its absolute path on the destination - the exact
/// failure mode that surfaces in the upstream `files-from.test` SSH-push
/// invocation.
///
/// # Upstream Reference
///
/// - `options.c:2473` - `filesfrom_fd = 0` for `--files-from=-` (stdin).
/// - `options.c:2501` - `filesfrom_fd = open(files_from, O_RDONLY|O_BINARY)`
///   for local files.
/// - `main.c:1322-1328` - remote files-from wires `filesfrom_fd = f_in` after
///   `setup_protocol()`; the remote receiver forwards the list bytes over the
///   wire via `start_filesfrom_forwarding`.
fn apply_files_from_for_sender(config: &ClientConfig, server_config: &mut ServerConfig) {
    use super::super::config::FilesFromSource;
    match config.files_from() {
        FilesFromSource::None => {}
        FilesFromSource::Stdin => {
            server_config.file_selection.files_from_path = Some("-".to_owned());
            server_config.file_selection.from0 = config.from0();
        }
        FilesFromSource::LocalFile(path) => {
            server_config.file_selection.files_from_path =
                Some(path.to_string_lossy().into_owned());
            server_config.file_selection.from0 = config.from0();
        }
        FilesFromSource::RemoteFile(_) => {
            // The remote receiver opens the file and forwards its bytes back
            // to us; the generator reads them as if they came from stdin.
            // upstream: main.c:1191-1198 start_filesfrom_forwarding(filesfrom_fd)
            server_config.file_selection.files_from_path = Some("-".to_owned());
            server_config.file_selection.from0 = true;
        }
    }
}

/// Returns `true` when both rsync wire compression and SSH stream
/// compression are enabled and a warning should be emitted.
///
/// Extracted so the predicate can be exercised independently of the
/// process-global `OnceLock` that suppresses duplicate warnings.
const fn should_warn_double_compression(rsync_compress: bool, ssh_compress: bool) -> bool {
    rsync_compress && ssh_compress
}

/// Emits a one-time warning to stderr when SSH built-in compression and
/// rsync `--compress` are both active.
///
/// Compressing twice wastes CPU and may expand already-compressed data,
/// since SSH cannot meaningfully recompress the rsync stream. Upstream
/// rsync does not detect this case; this is an oc-rsync usability
/// enhancement.
///
/// Detection is conservative: it only inspects the SSH command-line
/// arguments built up from `-e` / `--rsh` and friends, and does not parse
/// `~/.ssh/config`, since that file is merged at spawn time and we cannot
/// reliably read it. SSC-3 (#2563) tracks future `ssh_config` parsing.
///
/// The warning is suppressed after the first emission via a process-wide
/// `OnceLock<bool>`, so callers may invoke this once per SSH spawn without
/// flooding stderr.
fn warn_double_compression_once(rsync_compress: bool, ssh_compress: bool) {
    static EMITTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !should_warn_double_compression(rsync_compress, ssh_compress) {
        return;
    }
    EMITTED.get_or_init(|| {
        eprintln!("warning: both rsync wire compression (--compress) and SSH stream compression");
        eprintln!("         (-C in your ssh command) are enabled. Compressed data will be");
        eprintln!("         re-compressed by SSH, wasting CPU. Recommend dropping one of them.");
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_receiver_server_config() {
        let config = ClientConfig::builder().recursive(true).times(true).build();

        let result = build_server_config_for_receiver(&config, &["dest/".to_owned()]);
        assert!(result.is_ok());

        let server_config = result.unwrap();
        assert_eq!(server_config.role, ServerRole::Receiver);
        assert_eq!(server_config.args.len(), 1);
        assert_eq!(server_config.args[0], "dest/");
    }

    #[test]
    fn builds_generator_server_config() {
        let config = ClientConfig::builder().recursive(true).times(true).build();

        let result = build_server_config_for_generator(
            &config,
            &["file1.txt".to_owned(), "file2.txt".to_owned()],
        );
        assert!(result.is_ok());

        let server_config = result.unwrap();
        assert_eq!(server_config.role, ServerRole::Generator);
        assert_eq!(server_config.args.len(), 2);
        assert_eq!(server_config.args[0], "file1.txt");
        assert_eq!(server_config.args[1], "file2.txt");
    }

    /// UTS files-from SSH push regression: the local sender (Generator) must
    /// learn the local `--files-from` path so its generator reads entry names
    /// from the requested list instead of recursing the source operand and
    /// (under implied `--relative`) mirroring its absolute path on the
    /// destination.
    ///
    /// upstream: `options.c:2501 filesfrom_fd = open(files_from, ...)`,
    /// `flist.c:2275-2298` send_file_list() walking the open fd.
    #[test]
    fn generator_config_sets_files_from_path_for_local_file_push() {
        use super::super::super::config::FilesFromSource;
        use std::path::PathBuf;

        let list_path = PathBuf::from("/tmp/filelist.txt");
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::LocalFile(list_path.clone()))
            .build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some(list_path.to_string_lossy().as_ref()),
            "SSH push must point the local generator at the local --files-from \
             file so entries are emitted with relative wire-side names"
        );
    }

    /// SSH push with stdin-sourced `--files-from`: the local sender reads
    /// filenames from its standard input. The transfer crate signals this with
    /// the sentinel path "-" mirroring upstream's `options.c:2473
    /// filesfrom_fd = 0` assignment.
    #[test]
    fn generator_config_sets_files_from_path_for_stdin_push() {
        use super::super::super::config::FilesFromSource;

        let config = ClientConfig::builder()
            .files_from(FilesFromSource::Stdin)
            .from0(true)
            .build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("-")
        );
        assert!(server_config.file_selection.from0);
    }

    /// SSH push with remote-sourced `--files-from`: the local sender consumes
    /// the list bytes forwarded by the remote receiver over the wire. The
    /// transfer crate's protocol stream is the "-" sentinel here too;
    /// upstream wires this via `main.c:1322-1328 filesfrom_fd = f_in`.
    #[test]
    fn generator_config_sets_files_from_stdin_for_remote_push() {
        use super::super::super::config::FilesFromSource;

        let config = ClientConfig::builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("-"),
            "remote --files-from is read from the wire on the local sender"
        );
        assert!(server_config.file_selection.from0);
    }

    /// SSH push baseline: when `--files-from` is not configured the generator
    /// performs its usual recursive walk. The `files_from_path` field stays
    /// empty so the engine falls back to `build_file_list(paths)`.
    #[test]
    fn generator_config_leaves_files_from_path_unset_when_disabled() {
        let config = ClientConfig::builder().recursive(true).build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert!(
            server_config.file_selection.files_from_path.is_none(),
            "no --files-from must leave files_from_path unset"
        );
    }

    #[test]
    fn warns_on_double_compression() {
        // Both rsync --compress and SSH -C engaged: the predicate fires.
        assert!(should_warn_double_compression(true, true));
        // The one-shot emitter is safe to call; the first eligible call wins
        // process-wide and subsequent calls become no-ops. We only assert that
        // it does not panic or hang.
        warn_double_compression_once(true, true);
        warn_double_compression_once(true, true);
    }

    #[test]
    fn no_warning_when_only_rsync_compress() {
        assert!(!should_warn_double_compression(true, false));
        // Calling the emitter must be a no-op (no panic, no state change).
        warn_double_compression_once(true, false);
    }

    #[test]
    fn no_warning_when_only_ssh_compress() {
        assert!(!should_warn_double_compression(false, true));
        warn_double_compression_once(false, true);
    }

    #[test]
    fn no_warning_when_neither_compresses() {
        assert!(!should_warn_double_compression(false, false));
        warn_double_compression_once(false, false);
    }

    #[test]
    fn format_stderr_context_empty_input() {
        assert_eq!(format_stderr_context(&[]), "");
    }

    #[test]
    fn format_stderr_context_whitespace_only() {
        assert_eq!(format_stderr_context(b"  \n\n  "), "");
    }

    #[test]
    fn format_stderr_context_single_line() {
        let output = format_stderr_context(b"Permission denied (publickey).\n");
        assert_eq!(output, "\nSSH stderr:\nPermission denied (publickey).");
    }

    #[test]
    fn format_stderr_context_multi_line() {
        let input = b"Warning: Permanently added 'host' to known hosts.\nrsync error: some error\n";
        let output = format_stderr_context(input);
        assert!(output.starts_with("\nSSH stderr:\n"));
        assert!(output.contains("Warning: Permanently added"));
        assert!(output.contains("rsync error: some error"));
    }

    #[test]
    fn format_stderr_context_invalid_utf8() {
        let input = b"error: \xff\xfe bad bytes\n";
        let output = format_stderr_context(input);
        assert!(output.starts_with("\nSSH stderr:\n"));
        assert!(output.contains("error:"));
    }

    #[cfg(unix)]
    mod child_exit_status_tests {
        use super::*;
        use crate::exit_code::ExitCode;

        #[cfg(unix)]
        fn exit_status_for_code(code: i32) -> std::process::ExitStatus {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("exit {code}"))
                .status()
                .expect("failed to run sh")
        }

        #[cfg(unix)]
        #[test]
        fn maps_success_to_ok() {
            let status = exit_status_for_code(0);
            assert_eq!(map_child_exit_status(status), ExitCode::Ok);
        }

        #[cfg(unix)]
        #[test]
        fn maps_exit_127_to_command_not_found() {
            let status = exit_status_for_code(127);
            assert_eq!(map_child_exit_status(status), ExitCode::CommandNotFound);
        }

        #[cfg(unix)]
        #[test]
        fn maps_exit_126_to_command_run() {
            let status = exit_status_for_code(126);
            assert_eq!(map_child_exit_status(status), ExitCode::CommandRun);
        }

        #[cfg(unix)]
        #[test]
        fn maps_exit_255_to_command_failed() {
            let status = exit_status_for_code(255);
            assert_eq!(map_child_exit_status(status), ExitCode::CommandFailed);
        }

        #[cfg(unix)]
        #[test]
        fn maps_rsync_exit_code_23_to_partial_transfer() {
            let status = exit_status_for_code(23);
            assert_eq!(map_child_exit_status(status), ExitCode::PartialTransfer);
        }

        #[cfg(unix)]
        #[test]
        fn maps_rsync_exit_code_24_to_vanished() {
            let status = exit_status_for_code(24);
            assert_eq!(map_child_exit_status(status), ExitCode::Vanished);
        }

        #[cfg(unix)]
        #[test]
        fn maps_unknown_exit_code_to_partial_transfer() {
            let status = exit_status_for_code(42);
            assert_eq!(map_child_exit_status(status), ExitCode::PartialTransfer);
        }

        #[cfg(unix)]
        #[test]
        fn maps_signal_killed_to_command_killed() {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg("kill -9 $$")
                .spawn()
                .expect("spawn");
            let status = child.wait().expect("wait");
            assert_eq!(map_child_exit_status(status), ExitCode::CommandKilled);
        }
    }
}
