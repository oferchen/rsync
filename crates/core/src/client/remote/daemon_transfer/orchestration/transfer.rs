//! Pull and push transfer execution for daemon connections.
//!
//! Orchestrates the transfer lifecycle by configuring server infrastructure,
//! establishing the handshake result, and delegating to `run_server_with_handshake`.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use protocol::ProtocolVersion;

use super::server_config::{build_server_config_for_generator, build_server_config_for_receiver};
use super::stats::convert_server_stats_to_summary;
use crate::client::config::ClientConfig;
use crate::client::error::{ClientError, invalid_argument_error, remote_exit_error};
use crate::client::module_list::{
    DaemonStreamGuard, DaemonStreamReader, DaemonStreamWriter, build_io_timeout_reapply,
};
use crate::client::progress::ClientProgressObserver;
use crate::client::remote::batch_support::{BatchContext, build_batch_recording};
use crate::client::remote::flags;
use crate::client::remote::implied_source::implied_source_args_for_pull;
use crate::client::summary::ClientSummary;
use crate::exit_code::ExitCode;
use crate::message::Role;
use crate::server::handshake::HandshakeResult;
use crate::server::{TransferProgressCallback, TransferProgressEvent};

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
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_pull_transfer(
    config: &ClientConfig,
    reader: &mut DaemonStreamReader,
    writer: &mut DaemonStreamWriter,
    _guard: DaemonStreamGuard,
    local_paths: &[String],
    implied_source_args: &[String],
    protocol: ProtocolVersion,
    batch_ctx: Option<BatchContext>,
    buffered: Vec<u8>,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    let filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded())?;

    // upstream: compat.c:599 - protocol was negotiated via @RSYNCD text exchange,
    // setup_protocol() skips the binary exchange because remote_protocol != 0.
    let mut handshake = build_daemon_handshake(config, protocol);
    handshake.buffered = buffered;

    let mut server_config = build_server_config_for_receiver(config, local_paths, filter_rules)?;

    // upstream: main.c:1354-1356 - when pulling with --files-from pointing to a
    // local file or stdin, the client reads the file list locally and forwards
    // it to the daemon's generator over the protocol stream.
    if config
        .files_from()
        .resolve_for(false, config.from0())
        .stage_local_bytes
    {
        let data =
            crate::client::remote::files_from_forwarding::read_local_files_from_for_forwarding(
                config,
            )?;
        server_config.connection.files_from_data = Some(data);
    }

    // upstream: main.c:1549 / io.c:427,464 / flist.c:1026 - the requested daemon
    // source (module/path), or each local --files-from entry, is recorded as an
    // implied include; the receiver rejects any file-list name it does not cover
    // (CVE-2022-29154). is_daemon_connection drives the module-name strip on the
    // receiver side (exclude.c:396-401).
    server_config.connection.implied_source_args = implied_source_args_for_pull(
        config,
        implied_source_args,
        server_config.connection.files_from_data.as_deref(),
    );

    // Pull: local side is Receiver; batch records incoming data (is_sender=false).
    let batch_recording = batch_ctx
        .as_ref()
        .map(|ctx| build_batch_recording(ctx, false));

    let start = Instant::now();
    let mut adapter = observer.map(|obs| DaemonProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);

    // upstream: io.c:1551-1561 - the daemon sends MSG_IO_TIMEOUT once, right
    // after io_start_multiplex_out (main.c:1267-1268). As the client receiver we
    // adopt it and re-apply to the live socket. Build the re-apply hook from the
    // split socket halves; connect-program (pipe) transports yield None.
    let io_timeout_reapply = build_io_timeout_reapply(reader, writer);
    let server_stats = crate::server::run_server_with_handshake_adopting(
        server_config,
        handshake,
        reader,
        writer,
        crate::server::ServerTransferHooks {
            progress,
            batch: batch_recording,
            itemize: None,
            io_timeout_reapply,
        },
    )
    .map_err(|e| map_server_transfer_error(e, Role::Receiver))?;
    let elapsed = start.elapsed();

    let mut summary = convert_server_stats_to_summary(server_stats, elapsed);
    summary.set_protocol_version(protocol.as_u8());
    Ok(summary)
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_push_transfer(
    config: &ClientConfig,
    reader: &mut DaemonStreamReader,
    writer: &mut DaemonStreamWriter,
    _guard: DaemonStreamGuard,
    local_paths: &[String],
    protocol: ProtocolVersion,
    batch_ctx: Option<BatchContext>,
    buffered: Vec<u8>,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    let filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded())?;

    // upstream: compat.c:599 - if (remote_protocol == 0) { ... }
    let mut handshake = build_daemon_handshake(config, protocol);
    handshake.buffered = buffered;

    let server_config = build_server_config_for_generator(config, local_paths, filter_rules)?;
    let dry_run = config.dry_run();

    // Push: local side is Generator (sender); batch records outgoing data (is_sender=true).
    let batch_recording = batch_ctx
        .as_ref()
        .map(|ctx| build_batch_recording(ctx, true));

    let start = Instant::now();
    let mut adapter = observer.map(|obs| DaemonProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);

    // upstream: sender.c:461 log_item(FCLIENT) - on a push the client is the
    // sender, and the client-visible itemize row is printed by the SENDER from
    // the iflags the remote receiver's generator writes over the wire
    // (generator.c:583-599 write_shortint(sock_f_out, iflags) for protocol >=
    // 29). The remote generator's own log_item never reaches the client because
    // log.c:822 gates the FCLIENT write on `!am_server`. So the local sender is
    // the single source of push itemize; the remote receiver must not forward a
    // pre-rendered MSG_INFO line (see receiver::emit_itemize). This restores
    // output for oc-client -> upstream-daemon pushes, where upstream never
    // forwards oc's itemize.
    let wants_itemize = config.itemize_changes();
    let stdout_handle = std::io::stdout();
    let mut itemize_cb = move |line: &str| {
        let mut out = stdout_handle.lock();
        let _ = out.write_all(line.as_bytes());
    };

    let result = crate::server::run_server_with_handshake(
        server_config,
        handshake,
        reader,
        writer,
        progress,
        batch_recording,
        if wants_itemize {
            Some(&mut itemize_cb as &mut dyn crate::server::ItemizeCallback)
        } else {
            None
        },
    );

    match result {
        Ok(server_stats) => {
            let elapsed = start.elapsed();
            let mut summary = convert_server_stats_to_summary(server_stats, elapsed);
            summary.set_protocol_version(protocol.as_u8());
            Ok(summary)
        }
        Err(ref e) if dry_run && is_dry_run_remote_close(e) => {
            // upstream: clientserver.c - during --dry-run push, the daemon closes
            // its socket early after receiving the file list.
            Ok(ClientSummary::default())
        }
        Err(e) => Err(map_server_transfer_error(e, Role::Sender)),
    }
}

/// Maps a `run_server_with_handshake` failure to a [`ClientError`], honouring a
/// remote peer's explicit `MSG_ERROR_EXIT` code when one is present.
///
/// When the remote daemon rejects the transfer (for example a read-only module
/// on a push), it emits its error text via `MSG_ERROR_XFER` - already printed to
/// stderr by the multiplex reader - followed by `MSG_ERROR_EXIT` carrying the
/// exit code. The reader surfaces that code as a [`transfer::RemoteExitError`]
/// nested inside the returned `io::Error`. Recovering it here lets the client
/// exit with the daemon's own code (e.g. `RERR_SYNTAX = 1`) instead of forcing a
/// generic partial-transfer (23). The role-tagged `rsync error:` trailer mirrors
/// upstream `log_exit()`; the daemon's message is not reprinted because the
/// reader already delivered it to stderr in wire order.
///
/// Failures with no embedded remote code (local I/O, protocol desync) keep the
/// prior generic `transfer failed: ...` (23) diagnostic.
///
/// upstream: io.c:1663-1701 - `MSG_ERROR_EXIT` drives the NORETURN
/// `_exit_cleanup(val)`, so the client's final exit code is the peer's code.
fn map_server_transfer_error(error: std::io::Error, role: Role) -> ClientError {
    if let Some(code) = remote_exit_code(&error) {
        let exit = ExitCode::from_i32(code).unwrap_or(ExitCode::PartialTransfer);
        return remote_exit_error(exit, role, "");
    }
    invalid_argument_error(&format!("transfer failed: {error}"), 23)
}

/// Walks the source chain of an `io::Error` looking for a
/// [`transfer::RemoteExitError`], returning the peer-supplied exit code when the
/// failure originated from a remote `MSG_ERROR_EXIT` frame.
fn remote_exit_code(error: &std::io::Error) -> Option<i32> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error.get_ref()?);
    while let Some(err) = source {
        if let Some(remote) = err.downcast_ref::<crate::server::RemoteExitError>() {
            return Some(remote.code);
        }
        source = err.source();
    }
    None
}

/// Builds a `HandshakeResult` for daemon transfers where the protocol version
/// was already negotiated via the `@RSYNCD` text exchange.
///
/// upstream: compat.c:599 - when `remote_protocol != 0`, `setup_protocol()`
/// skips the binary version exchange.
fn build_daemon_handshake(config: &ClientConfig, protocol: ProtocolVersion) -> HandshakeResult {
    HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: config.timeout().as_seconds().map(|s| s.get()),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
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
pub(super) fn is_dry_run_remote_close(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::WouldBlock
    )
}

/// Adapts a [`ClientProgressObserver`] to [`TransferProgressCallback`].
///
/// Converts server-side per-file progress events into client-side progress
/// updates, enabling live progress display during daemon transfers. Mirrors
/// the `ServerProgressAdapter` in the SSH transfer path.
///
/// upstream: progress.c - `end_progress()` is called after each file completes,
/// updating cumulative bytes and triggering the progress display.
pub(super) struct DaemonProgressAdapter<'a> {
    observer: &'a mut dyn ClientProgressObserver,
    start: Instant,
    overall_transferred: u64,
}

impl<'a> DaemonProgressAdapter<'a> {
    pub(super) fn new(observer: &'a mut dyn ClientProgressObserver, start: Instant) -> Self {
        Self {
            observer,
            start,
            overall_transferred: 0,
        }
    }
}

impl TransferProgressCallback for DaemonProgressAdapter<'_> {
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>) {
        self.overall_transferred += event.file_bytes;

        let client_event = crate::client::summary::ClientEvent::from_progress(
            event.path,
            event.file_bytes,
            event.total_file_bytes,
            self.start.elapsed(),
            Arc::from(Path::new("")),
        );

        let update = crate::client::progress::ClientProgressUpdate::from_transfer_event(
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

/// Reads the `--files-from` source and serializes it into the wire format
/// for forwarding to a remote daemon.
///
/// Re-export of the shared helper - see
/// [`crate::client::remote::files_from_forwarding::read_local_files_from_for_forwarding`]
/// for the implementation.
///
/// # Upstream Reference
///
/// - `io.c:forward_filesfrom_data()` - reads from local fd, writes to socket
/// - `main.c:1354-1356` - `start_filesfrom_forwarding(filesfrom_fd)`
#[cfg(test)]
pub(super) use crate::client::remote::files_from_forwarding::read_local_files_from_for_forwarding as read_files_from_for_forwarding;

#[cfg(test)]
mod map_server_transfer_error_tests {
    use super::*;
    use crate::server::RemoteExitError;

    fn remote_exit_io(code: i32) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            RemoteExitError { code },
        )
    }

    /// The production path wraps `RemoteExitError` directly as the inner error,
    /// so it must be recovered from `get_ref()`.
    #[test]
    fn extracts_code_from_direct_inner_error() {
        assert_eq!(remote_exit_code(&remote_exit_io(1)), Some(1));
    }

    /// A `RemoteExitError` reached only via a `source()` link must still be
    /// found, so an intermediate wrapper cannot hide the peer's code.
    #[test]
    fn extracts_code_from_nested_source_chain() {
        #[derive(Debug)]
        struct Wrap(RemoteExitError);
        impl std::fmt::Display for Wrap {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "wrapped: {}", self.0)
            }
        }
        impl std::error::Error for Wrap {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let nested = std::io::Error::other(Wrap(RemoteExitError { code: 7 }));
        assert_eq!(remote_exit_code(&nested), Some(7));
    }

    #[test]
    fn no_code_for_plain_io_error() {
        let plain = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe");
        assert_eq!(remote_exit_code(&plain), None);
    }

    /// A daemon rejection (e.g. read-only module, RERR_SYNTAX = 1) must exit
    /// with the daemon's code, tagged with the local role, and must NOT prepend
    /// the generic `transfer failed:` prefix - the reader already printed the
    /// daemon's message to stderr in wire order.
    #[test]
    fn maps_remote_reject_to_daemon_code_without_transfer_failed_prefix() {
        let err = map_server_transfer_error(remote_exit_io(1), Role::Sender);
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.code(), ExitCode::Syntax);
        let rendered = err.to_string();
        assert!(!rendered.contains("transfer failed"), "{rendered}");
        assert!(rendered.contains("[sender="), "{rendered}");
    }

    /// A pull tags the diagnostic with the receiver role.
    #[test]
    fn maps_remote_reject_pull_uses_receiver_role() {
        let err = map_server_transfer_error(remote_exit_io(1), Role::Receiver);
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("[receiver="), "{err}");
    }

    /// Failures with no embedded remote code keep the prior generic
    /// `transfer failed: ...` (23) behaviour.
    #[test]
    fn maps_plain_failure_to_generic_partial_transfer() {
        let err = map_server_transfer_error(
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe"),
            Role::Receiver,
        );
        assert_eq!(err.exit_code(), 23);
        assert!(err.to_string().contains("transfer failed"), "{err}");
    }
}
