//! Daemon transfer orchestration.
//!
//! Coordinates daemon-based remote transfers (rsync:// URLs) by connecting to
//! rsync daemons, performing handshakes, and executing transfers using the
//! server infrastructure. This mirrors the flow in upstream
//! `clientserver.c:start_inband_exchange()` and
//! `clientserver.c:start_daemon_client()`.
//!
//! Split into submodules by responsibility:
//! - `connection` - connection establishment, authentication, early-input
//! - `orchestration` - argument building, transfer execution, server config
//!
//! # Upstream Reference
//!
//! - `clientserver.c:start_daemon_client()` - Daemon connection entry point
//! - `clientserver.c:start_inband_exchange()` - Module selection and auth
//! - `authenticate.c` - Challenge/response authentication
//! - `socket.c:open_socket_out()` - TCP connection establishment

mod connection;
mod orchestration;

#[cfg(feature = "tracing")]
use tracing::instrument;

use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine::batch::BatchWriter;

use super::super::DAEMON_SOCKET_TIMEOUT;
use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error, socket_error};
use super::super::module_list::{
    RshDaemonSpawn, open_daemon_stream, resolve_connect_timeout, spawn_rsh_daemon_stream,
};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::batch_support::build_batch_context;
use super::invocation::{RemoteRole, TransferSpec, determine_transfer_role};

use connection::{DaemonTransferRequest, perform_daemon_handshake};
use orchestration::{run_pull_transfer, run_push_transfer, send_daemon_arguments};

/// Executes a transfer over daemon protocol (rsync://).
///
/// Entry point for daemon-based remote transfers, mirroring upstream
/// `clientserver.c:start_daemon_client()`:
/// 1. Parses the rsync:// URL or double-colon operand
/// 2. Connects to the daemon (upstream: `socket.c:open_socket_out()`)
/// 3. Performs the daemon handshake (upstream: `clientserver.c:start_inband_exchange()`)
/// 4. Sends arguments to daemon
/// 5. Determines role from operand positions
/// 6. Executes the transfer using server infrastructure
///
/// Supports `RSYNC_CONNECT_PROG` for piped connections (used by upstream
/// testsuite) and double-colon syntax (`host::module/path`).
#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "daemon_transfer")
)]
pub fn run_daemon_transfer(
    config: &ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    let args = config.transfer_args();
    // upstream: options.c:2194 - a single source with list_only set lists the
    // module contents (`host::module` with no destination); only a genuinely
    // empty operand list is an error.
    if args.is_empty() || (args.len() < 2 && !config.list_only()) {
        return Err(invalid_argument_error(
            "need at least one source and one destination",
            1,
        ));
    }

    // For an implicit list-only transfer (single source, no dest) synthesize a
    // dummy `.` destination; config.list_only() forces read-only/no-write so it
    // is never materialized.
    let dummy_dest = std::ffi::OsString::from(".");
    let (sources, destination) = if args.len() < 2 {
        (args, &dummy_dest)
    } else {
        let (sources, destination) = args.split_at(args.len() - 1);
        (sources, &destination[0])
    };

    let transfer_spec = determine_transfer_role(sources, destination)?;
    let role = transfer_spec.role();
    let local_paths = match &transfer_spec {
        TransferSpec::Push { local_sources, .. } => local_sources.clone(),
        TransferSpec::Pull { local_dest, .. } => vec![local_dest.clone()],
        TransferSpec::Proxy { .. } => {
            return Err(invalid_argument_error(
                "remote-to-remote transfers via rsync daemon are not supported",
                1,
            ));
        }
    };

    let daemon_operand = config
        .transfer_args()
        .iter()
        .find(|arg| {
            let s = arg.to_string_lossy();
            s.starts_with("rsync://") || s.starts_with("RSYNC://") || s.contains("::")
        })
        .ok_or_else(|| invalid_argument_error("no daemon URL or host::module operand found", 1))?;

    let daemon_operand_str = daemon_operand.to_string_lossy();
    let request = if daemon_operand_str.starts_with("rsync://")
        || daemon_operand_str.starts_with("RSYNC://")
    {
        DaemonTransferRequest::parse_rsync_url(&daemon_operand_str)?
    } else {
        DaemonTransferRequest::parse_double_colon(&daemon_operand_str)?
    };

    // upstream: socket.c:274-277 - open_socket_out() bounds connect(2) only when
    // --contimeout is set; --timeout never bounds the connect phase.
    let connect_duration = resolve_connect_timeout(config.connect_timeout());
    let handshake_io_timeout = config.timeout().effective(DAEMON_SOCKET_TIMEOUT);
    let stream = open_daemon_stream(
        &request.address,
        connect_duration,
        handshake_io_timeout,
        config.address_mode(),
        config.connect_program(),
        config.bind_address().map(|b| b.socket()),
        config.tcp_fastopen(),
        config.sockopts(),
    )?;

    // upstream: socket.c:279-280 - set_socket_options(s, sockopts) is applied
    // pre-connect inside open_daemon_stream, before start_daemon_client()'s
    // handshake. Only applies to TCP connections, not connect programs (that
    // gate lives inside connect_direct/connect_via_proxy).

    // Apply oc-rsync-specific TCP perf options (TCP_NOTSENT_LOWAT for the
    // client side; client-side TFO is deferred to a follow-up). These are
    // wire-compatible with upstream and only affect kernel socket state.
    if let Some(tcp) = stream.as_tcp_stream() {
        // Mirror `--bwlimit` as a kernel pacing hint (saturating to the
        // SO_MAX_PACING_RATE u32 field); the userspace limiter stays
        // authoritative.
        let pacing = config
            .bandwidth_limit()
            .map(|limit| u32::try_from(limit.bytes_per_second().get()).unwrap_or(u32::MAX));
        crate::client::module_list::tcp_perf::apply_client_tcp_perf_options(
            tcp,
            config.tcp_fastopen(),
            pacing,
        );
    }

    // upstream: io.c - select_timeout() uses io_timeout for all transfer I/O.
    // Configure TCP_NODELAY and transfer-phase timeouts before splitting.
    // For TCP, settings apply to both halves (shared underlying socket).
    // For connect programs, this is a no-op.
    let transfer_timeout = config
        .timeout()
        .as_seconds()
        .map(|s| std::time::Duration::from_secs(s.get()));
    stream
        .configure_transfer_options(true, transfer_timeout)
        .map_err(|e| socket_error("configure transfer options on", "daemon socket", e))?;

    // Split the stream into read/write halves for the handshake. The line-based
    // @RSYNCD protocol needs a BufReader on the read side while simultaneously
    // writing responses on the write side.
    let (reader_half, mut writer_half, guard) = stream
        .split()
        .map_err(|e| socket_error("split daemon stream for", "handshake", e))?;
    let mut buf_reader = BufReader::new(reader_half);

    let output_motd = !config.no_motd();
    let protocol = perform_daemon_handshake(
        &mut buf_reader,
        &mut writer_half,
        &request,
        output_motd,
        config.daemon_params(),
        config.early_input(),
        config.protocol_version(),
        config.password_override(),
    )?;

    // For pull (we receive), the daemon is the sender, so is_sender=true.
    // For push (we send), the daemon is the receiver, so is_sender=false.
    let daemon_is_sender = matches!(role, RemoteRole::Receiver);
    send_daemon_arguments(
        &mut writer_half,
        config,
        &request,
        protocol,
        daemon_is_sender,
    )?;

    let batch_ctx = batch_writer.map(|bw| build_batch_context(config, bw));

    // Extract any bytes the BufReader buffered beyond the last handshake line.
    // These bytes are the start of the binary transfer protocol and must be
    // chained ahead of the reader in the transfer functions.
    let buffered = buf_reader.buffer().to_vec();
    let mut reader_half = buf_reader.into_inner();

    // Protocol is already negotiated via @RSYNCD text exchange (not binary 4-byte).
    // upstream: compat.c:599 - when remote_protocol != 0, setup_protocol skips
    // the binary exchange.
    // upstream: main.c:1549 - record the requested daemon source (module/path)
    // as an implied include for the receiver-side flist validation
    // (CVE-2022-29154); is_daemon_connection strips the module on the receiver.
    let implied_source_args = [format!("{}/{}", request.module, request.path)];
    match role {
        RemoteRole::Receiver => run_pull_transfer(
            config,
            &mut reader_half,
            &mut writer_half,
            guard,
            &local_paths,
            &implied_source_args,
            protocol,
            batch_ctx,
            buffered,
            observer,
        ),
        RemoteRole::Sender => run_push_transfer(
            config,
            &mut reader_half,
            &mut writer_half,
            guard,
            &local_paths,
            protocol,
            batch_ctx,
            buffered,
            observer,
        ),
        RemoteRole::Proxy => {
            unreachable!("Proxy transfers via daemon are rejected earlier")
        }
    }
}

/// Executes a daemon transfer tunneled over a remote shell (SSH with `::` syntax).
///
/// Mirrors upstream `main.c:1577-1586`: when `-e`/`--rsh` is active with a
/// double-colon operand, the client spawns the remote shell with
/// `rsync --server --daemon .` as the remote command, then speaks the
/// `@RSYNCD:` daemon protocol over the shell's stdio pipes.
///
/// This avoids opening a direct TCP connection to the rsync daemon port -
/// instead the daemon runs on the remote end as a child of the SSH session,
/// communicating via stdin/stdout.
#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "daemon_over_remote_shell")
)]
pub fn run_daemon_over_remote_shell(
    config: &ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    let args = config.transfer_args();
    // upstream: options.c:2194 - a single source with list_only set lists the
    // module contents (`host::module` with no destination); only a genuinely
    // empty operand list is an error.
    if args.is_empty() || (args.len() < 2 && !config.list_only()) {
        return Err(invalid_argument_error(
            "need at least one source and one destination",
            1,
        ));
    }

    // For an implicit list-only transfer (single source, no dest) synthesize a
    // dummy `.` destination; config.list_only() forces read-only/no-write so it
    // is never materialized.
    let dummy_dest = std::ffi::OsString::from(".");
    let (sources, destination) = if args.len() < 2 {
        (args, &dummy_dest)
    } else {
        let (sources, destination) = args.split_at(args.len() - 1);
        (sources, &destination[0])
    };

    let transfer_spec = determine_transfer_role(sources, destination)?;
    let role = transfer_spec.role();
    let local_paths = match &transfer_spec {
        TransferSpec::Push { local_sources, .. } => local_sources.clone(),
        TransferSpec::Pull { local_dest, .. } => vec![local_dest.clone()],
        TransferSpec::Proxy { .. } => {
            return Err(invalid_argument_error(
                "remote-to-remote transfers via daemon-over-remote-shell are not supported",
                1,
            ));
        }
    };

    let daemon_operand = config
        .transfer_args()
        .iter()
        .find(|arg| arg.to_string_lossy().contains("::"))
        .ok_or_else(|| invalid_argument_error("no host::module operand found", 1))?;
    let daemon_operand_str = daemon_operand.to_string_lossy();
    let request = DaemonTransferRequest::parse_double_colon(&daemon_operand_str)?;

    // upstream: main.c:594-604 - when daemon_connection > 0, the remote
    // command is `rsync_path --server --daemon .` with no server_options().
    let shell_args = config
        .remote_shell()
        .ok_or_else(|| invalid_argument_error("daemon-over-remote-shell requires -e/--rsh", 1))?;

    // Shared with the module-listing path so `-e PROG host::` behaves
    // identically whether listing modules or transferring files.
    let stream = spawn_rsh_daemon_stream(RshDaemonSpawn {
        shell_args,
        host: request.address.host(),
        username: request.username.as_deref(),
        port: request.address.port(),
        rsync_path: config.rsync_path(),
        bind_address: config.bind_address().map(|addr| addr.socket().ip()),
        jump_hosts: config.jump_hosts(),
        connect_timeout: config.connect_timeout().effective(Duration::from_secs(30)),
        address_mode: config.address_mode(),
    })?;

    let transfer_timeout = config
        .timeout()
        .as_seconds()
        .map(|s| Duration::from_secs(s.get()));
    stream
        .configure_transfer_options(true, transfer_timeout)
        .map_err(|e| socket_error("configure transfer options on", "remote shell stream", e))?;

    let (reader_half, mut writer_half, guard) = stream
        .split()
        .map_err(|e| socket_error("split remote shell stream for", "handshake", e))?;
    let mut buf_reader = BufReader::new(reader_half);

    let output_motd = !config.no_motd();
    let protocol = perform_daemon_handshake(
        &mut buf_reader,
        &mut writer_half,
        &request,
        output_motd,
        config.daemon_params(),
        config.early_input(),
        config.protocol_version(),
        config.password_override(),
    )?;

    let daemon_is_sender = matches!(role, RemoteRole::Receiver);
    send_daemon_arguments(
        &mut writer_half,
        config,
        &request,
        protocol,
        daemon_is_sender,
    )?;

    let batch_ctx = batch_writer.map(|bw| build_batch_context(config, bw));

    let buffered = buf_reader.buffer().to_vec();
    let mut reader_half = buf_reader.into_inner();

    // upstream: main.c:1549 - record the requested daemon source (module/path)
    // as an implied include for the receiver-side flist validation
    // (CVE-2022-29154); is_daemon_connection strips the module on the receiver.
    let implied_source_args = [format!("{}/{}", request.module, request.path)];
    match role {
        RemoteRole::Receiver => run_pull_transfer(
            config,
            &mut reader_half,
            &mut writer_half,
            guard,
            &local_paths,
            &implied_source_args,
            protocol,
            batch_ctx,
            buffered,
            observer,
        ),
        RemoteRole::Sender => run_push_transfer(
            config,
            &mut reader_half,
            &mut writer_half,
            guard,
            &local_paths,
            protocol,
            batch_ctx,
            buffered,
            observer,
        ),
        RemoteRole::Proxy => {
            unreachable!("Proxy transfers via daemon-over-remote-shell are rejected earlier")
        }
    }
}
