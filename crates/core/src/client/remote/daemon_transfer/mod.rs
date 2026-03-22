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

use super::super::DAEMON_SOCKET_TIMEOUT;
use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error};
use super::super::module_list::{apply_socket_options, connect_direct, resolve_connect_timeout};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::invocation::{RemoteRole, TransferSpec, determine_transfer_role};

use connection::{DaemonTransferRequest, perform_daemon_handshake};
use orchestration::{run_pull_transfer, run_push_transfer, send_daemon_arguments};

/// Executes a transfer over daemon protocol (rsync://).
///
/// Entry point for daemon-based remote transfers, mirroring upstream
/// `clientserver.c:start_daemon_client()`:
/// 1. Parses the rsync:// URL
/// 2. Connects to the daemon (upstream: `socket.c:open_socket_out()`)
/// 3. Performs the daemon handshake (upstream: `clientserver.c:start_inband_exchange()`)
/// 4. Sends arguments to daemon
/// 5. Determines role from operand positions
/// 6. Executes the transfer using server infrastructure
#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, _observer), name = "daemon_transfer")
)]
pub fn run_daemon_transfer(
    config: &ClientConfig,
    _observer: Option<&mut dyn ClientProgressObserver>,
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

    let daemon_url = config
        .transfer_args()
        .iter()
        .find(|arg| {
            let s = arg.to_string_lossy();
            s.starts_with("rsync://") || s.starts_with("RSYNC://")
        })
        .ok_or_else(|| invalid_argument_error("no rsync:// URL found", 1))?;

    let request = DaemonTransferRequest::parse_rsync_url(&daemon_url.to_string_lossy())?;

    // upstream: clientserver.c - start_daemon_client() applies io_timeout to connect.
    let connect_duration = resolve_connect_timeout(
        config.connect_timeout(),
        config.timeout(),
        DAEMON_SOCKET_TIMEOUT,
    );
    let handshake_io_timeout = config.timeout().effective(DAEMON_SOCKET_TIMEOUT);
    let mut stream = connect_direct(
        &request.address,
        connect_duration,
        handshake_io_timeout,
        config.address_mode(),
        config.bind_address().map(|b| b.socket()),
    )?;

    // upstream: clientserver.c - start_daemon_client() calls set_socket_options()
    // on the daemon socket before the handshake.
    if let Some(sockopts) = config.sockopts() {
        apply_socket_options(&stream, sockopts)?;
    }

    let output_motd = !config.no_motd();
    let protocol = perform_daemon_handshake(
        &mut stream,
        &request,
        output_motd,
        config.daemon_params(),
        config.early_input(),
        config.protocol_version(),
    )?;

    // For pull (we receive), the daemon is the sender, so is_sender=true.
    // For push (we send), the daemon is the receiver, so is_sender=false.
    let daemon_is_sender = matches!(role, RemoteRole::Receiver);
    send_daemon_arguments(&mut stream, config, &request, protocol, daemon_is_sender)?;

    // Protocol is already negotiated via @RSYNCD text exchange (not binary 4-byte).
    // upstream: compat.c:599 - when remote_protocol != 0, setup_protocol skips
    // the binary exchange.
    match role {
        RemoteRole::Receiver => run_pull_transfer(config, stream, &local_paths, protocol),
        RemoteRole::Sender => run_push_transfer(config, stream, &local_paths, protocol),
        RemoteRole::Proxy => {
            unreachable!("Proxy transfers via daemon are rejected earlier")
        }
    }
}
