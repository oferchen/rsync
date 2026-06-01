//! Daemon transfer orchestration.
//!
//! Coordinates daemon-based remote transfers (rsync:// URLs) by connecting to
//! rsync daemons, performing handshakes, and executing transfers using the
//! server infrastructure. This mirrors the flow in upstream
//! `clientserver.c:start_inband_exchange()` and
//! `clientserver.c:start_daemon_client()`.
//!
//! Split into submodules by responsibility:
//! - [`connection`] - connection establishment, authentication, early-input
//! - [`orchestration`] - argument building, transfer execution, server config
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

use engine::batch::BatchWriter;

use super::super::DAEMON_SOCKET_TIMEOUT;
use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error, socket_error};
use super::super::module_list::{
    apply_socket_options, open_daemon_stream, resolve_connect_timeout,
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
    instrument(skip(config, _observer), name = "daemon_transfer")
)]
pub fn run_daemon_transfer(
    config: &ClientConfig,
    _observer: Option<&mut dyn ClientProgressObserver>,
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

    // upstream: clientserver.c - start_daemon_client() applies io_timeout to connect.
    let connect_duration = resolve_connect_timeout(
        config.connect_timeout(),
        config.timeout(),
        DAEMON_SOCKET_TIMEOUT,
    );
    let handshake_io_timeout = config.timeout().effective(DAEMON_SOCKET_TIMEOUT);
    let stream = open_daemon_stream(
        &request.address,
        connect_duration,
        handshake_io_timeout,
        config.address_mode(),
        config.connect_program(),
        config.bind_address().map(|b| b.socket()),
    )?;

    // upstream: clientserver.c - start_daemon_client() calls set_socket_options()
    // on the daemon socket before the handshake. Only applies to TCP connections,
    // not connect programs.
    if let Some(sockopts) = config.sockopts() {
        if let Some(tcp) = stream.as_tcp_stream() {
            apply_socket_options(tcp, sockopts)?;
        }
    }

    // When the client requests TLS, wrap the TCP stream before the daemon
    // handshake. Full TLS transfer wiring is deferred to TLS-11; for now
    // the presence of a TLS config triggers an early error so the code
    // path is exercised by the compiler.
    #[cfg(feature = "client-tls")]
    if let Some(_tls_cfg) = config.tls_config() {
        return Err(invalid_argument_error(
            "client-side TLS transfers not yet supported",
            2,
        ));
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
    match role {
        RemoteRole::Receiver => run_pull_transfer(
            config,
            &mut reader_half,
            &mut writer_half,
            guard,
            &local_paths,
            protocol,
            batch_ctx,
            buffered,
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
        ),
        RemoteRole::Proxy => {
            unreachable!("Proxy transfers via daemon are rejected earlier")
        }
    }
}
