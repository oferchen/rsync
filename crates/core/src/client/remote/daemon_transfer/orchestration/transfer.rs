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
use crate::client::error::{ClientError, invalid_argument_error};
use crate::client::module_list::{DaemonStreamGuard, DaemonStreamReader, DaemonStreamWriter};
use crate::client::progress::ClientProgressObserver;
use crate::client::remote::batch_support::{BatchContext, build_batch_recording};
use crate::client::remote::flags;
use crate::client::summary::ClientSummary;
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
    if config.files_from().is_local_forwarded() {
        let data = read_files_from_for_forwarding(config)?;
        server_config.connection.files_from_data = Some(data);
    }

    // Pull: local side is Receiver; batch records incoming data (is_sender=false).
    let batch_recording = batch_ctx
        .as_ref()
        .map(|ctx| build_batch_recording(ctx, false));

    let start = Instant::now();
    let mut adapter = observer.map(|obs| DaemonProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);

    let server_stats = crate::server::run_server_with_handshake(
        server_config,
        handshake,
        reader,
        writer,
        progress,
        batch_recording,
        None,
    )
    .map_err(|e| invalid_argument_error(&format!("transfer failed: {e}"), 23))?;
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

    // upstream: log.c:330-340 - when !am_server, rwrite() sends itemize to
    // FCLIENT (stdout); the callback writes directly to process stdout.
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
            Ok(convert_server_stats_to_summary(server_stats, elapsed))
        }
        Err(ref e) if dry_run && is_dry_run_remote_close(e) => {
            // upstream: clientserver.c - during --dry-run push, the daemon closes
            // its socket early after receiving the file list.
            Ok(ClientSummary::default())
        }
        Err(e) => Err(invalid_argument_error(&format!("transfer failed: {e}"), 23)),
    }
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
pub(super) fn read_files_from_for_forwarding(
    config: &ClientConfig,
) -> Result<Vec<u8>, ClientError> {
    use crate::client::config::FilesFromSource;

    let eol_nulls = config.from0();
    // upstream: compat.c:799-806 - filesfrom_convert is set when
    // protect_args && files_from && (am_sender ? ic_send : ic_recv) != -1.
    // For pull, this peer is the receiver writing to the wire; the converter
    // transcodes from local charset to the UTF-8 wire encoding.
    let iconv_converter = if config.protect_args().unwrap_or(false) {
        config.iconv().resolve_converter()
    } else {
        None
    };
    let mut wire_data = Vec::new();

    match config.files_from() {
        FilesFromSource::Stdin => {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            protocol::forward_files_from(
                &mut reader,
                &mut wire_data,
                eol_nulls,
                iconv_converter.as_ref(),
            )
            .map_err(|e| {
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
            protocol::forward_files_from(
                &mut file,
                &mut wire_data,
                eol_nulls,
                iconv_converter.as_ref(),
            )
            .map_err(|e| {
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
