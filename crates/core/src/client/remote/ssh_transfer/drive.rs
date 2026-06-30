//! Transfer drive and orchestration.
//!
//! Holds the public entry point ([`run_ssh_transfer`]) and the push/pull/proxy
//! drivers, including the connection-splitting transfer loop that runs the
//! server over the SSH pipes and reaps the remote child. This mirrors the flow
//! in upstream `main.c:do_cmd()` / `main.c:client_run()`.

use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[cfg(feature = "tracing")]
use tracing::instrument;

use engine::batch::BatchWriter;
use rsync_io::ssh::SshConnection;

use super::super::super::config::ClientConfig;
use super::super::super::error::{
    ClientError, invalid_argument_error, invalid_argument_error_typed,
};
use super::super::super::progress::ClientProgressObserver;
use super::super::super::summary::ClientSummary;
use super::super::batch_support::{BatchContext, build_batch_context, build_batch_recording};
use super::super::files_from_forwarding::read_local_files_from_for_forwarding;
use super::super::flags;
use super::super::invocation::{RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role};
use super::connection::build_ssh_connection;
use super::exit_status::{
    convert_server_stats_to_summary, format_stderr_context, map_child_exit_status,
};
use super::parse::{parse_remote_operands, parse_single_remote};
use super::progress::ServerProgressAdapter;
use super::server_config::{build_server_config_for_generator, build_server_config_for_receiver};
use crate::exit_code::ExitCode;
use crate::server::{ServerConfig, ServerRole, TransferProgressCallback};

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
    if config
        .files_from()
        .resolve_for(false, config.from0())
        .stage_local_bytes
    {
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
    use super::super::remote_to_remote::run_remote_to_remote_transfer;

    run_remote_to_remote_transfer(config, remote_sources, remote_dest)
}

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
