//! Pull and push transfer execution for daemon connections.
//!
//! Orchestrates the transfer lifecycle by configuring server infrastructure,
//! establishing the handshake result, and delegating to `run_server_with_handshake`.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use protocol::ProtocolVersion;

use super::server_config::{build_server_config_for_generator, build_server_config_for_receiver};
use super::stats::convert_server_stats_to_summary;
use crate::client::config::ClientConfig;
use crate::client::error::{ClientError, remote_exit_error};
use crate::client::module_list::{
    DaemonStreamReader, DaemonStreamWriter, build_io_timeout_reapply, register_shutdown_wake,
};
use crate::client::progress::ClientProgressObserver;
use crate::client::remote::batch_support::{BatchContext, build_batch_recording};
use crate::client::remote::flags;
use crate::client::remote::implied_source::implied_source_args_for_pull;
use crate::client::remote::itemize_sink::ItemizeEventSink;
use crate::client::remote::reporting_side::{ReportingSide, local_failure_error};
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
    local_paths: &[OsString],
    implied_source_args: &[String],
    protocol: ProtocolVersion,
    batch_ctx: Option<BatchContext>,
    buffered: Vec<u8>,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    let filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded())?;

    // upstream: compat.c:602 - protocol was negotiated via @RSYNCD text exchange,
    // setup_protocol() skips the binary exchange because remote_protocol != 0.
    let mut handshake = build_daemon_handshake(config, protocol);
    handshake.buffered = buffered;
    let bytes_read = Arc::new(AtomicU64::new(0));
    // upstream: io.c:259-266 check_timeout() names the --timeout in force.
    let io_timeout = config.timeout().as_seconds().map(|s| s.get());

    let fork_point = Arc::new(crate::server::ForkPoint::default());
    let mut server_config = build_server_config_for_receiver(config, local_paths, filter_rules)?;

    // upstream: main.c:1428-1431 - when pulling with --files-from pointing to a
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

    // upstream: main.c:1581 / io.c:512,549 / flist.c:1368 - the requested daemon
    // source (module/path), or each local --files-from entry, is recorded as an
    // implied include; the receiver rejects any file-list name it does not cover
    // (CVE-2022-29154). is_daemon_connection drives the module-name strip on the
    // receiver side (exclude.c:494 add_implied_include()).
    server_config.connection.implied_source_args = implied_source_args_for_pull(
        config,
        implied_source_args,
        server_config.connection.files_from_data.as_deref(),
    );
    // upstream: exclude.c:476-494 / main.c:1581 - only a raw daemon module/path
    // operand is recorded with skip_daemon_module=1; forwarded --files-from
    // entries (io.c:512,549) are already module-relative and keep
    // skip_daemon_module=0. Record the decision now, from the stable
    // files-from-active signal, because files_from_data is later taken while
    // forwarding the list and would read as absent at validation time.
    server_config.connection.implied_skip_daemon_module =
        server_config.connection.is_daemon_connection && !config.files_from().is_active();

    // Pull: local side is Receiver; batch records incoming data (is_sender=false).
    let batch_recording = batch_ctx
        .as_ref()
        .map(|ctx| build_batch_recording(ctx, false));

    let start = Instant::now();
    let mut adapter = observer.map(|obs| DaemonProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);

    // upstream: main.c:1311-1313 - the daemon sends MSG_IO_TIMEOUT once, right
    // after io_start_multiplex_out; the client reads it in io.c:1750
    // read_a_msg(). As the client receiver we adopt it and re-apply to the
    // live socket. Build the re-apply hook from the split socket halves;
    // connect-program (pipe) transports yield None.
    let io_timeout_reapply = build_io_timeout_reapply(reader, writer);
    // Let an interrupt release a receive blocked on the wire. Held for the
    // duration of the transfer; dropping it deregisters the socket.
    let _wake_guard = register_shutdown_wake(reader);
    // A custom `--out-format` makes the receiver buffer metadata events (it
    // suppresses its own stdout); collect them for the CLI to render. Otherwise
    // the receiver prints its own default `-v`/`-i` output and no callback is
    // needed (mirrors the SSH pull driver).
    let render_out_format = config.render_out_format_locally();
    let mut itemize_sink = ItemizeEventSink::new(render_out_format);
    let itemize: Option<&mut dyn crate::server::ItemizeCallback> = if render_out_format {
        Some(&mut itemize_sink)
    } else {
        None
    };
    let server_stats = crate::server::run_server_with_handshake_adopting(
        server_config,
        handshake,
        reader,
        writer,
        crate::server::ServerTransferHooks {
            progress,
            batch: batch_recording,
            itemize,
            io_timeout_reapply,
            // The client end of a daemon transfer never writes the daemon's
            // module log; that is the remote daemon's own FLOG sink.
            daemon_log: None,
            bytes_read: Some(Arc::clone(&bytes_read)),
            fork_point: Some(Arc::clone(&fork_point)),
        },
    )
    .map_err(|e| {
        map_server_transfer_error(
            e,
            ReportingSide::of(Role::PreForkReceiver, &fork_point),
            bytes_read.load(Ordering::Relaxed),
            io_timeout,
        )
    })?;
    let elapsed = start.elapsed();

    let mut summary = convert_server_stats_to_summary(server_stats, elapsed);
    let events = itemize_sink.take_events();
    if !events.is_empty() {
        summary = summary.with_events(events);
    }
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
    local_paths: &[OsString],
    protocol: ProtocolVersion,
    batch_ctx: Option<BatchContext>,
    buffered: Vec<u8>,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    let filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded())?;

    // upstream: compat.c:602 - if (remote_protocol == 0) { ... }
    let mut handshake = build_daemon_handshake(config, protocol);
    handshake.buffered = buffered;
    let bytes_read = Arc::new(AtomicU64::new(0));
    // upstream: io.c:259-266 check_timeout() names the --timeout in force.
    let io_timeout = config.timeout().as_seconds().map(|s| s.get());

    let server_config = build_server_config_for_generator(config, local_paths, filter_rules)?;
    let dry_run = config.dry_run();

    // Let an interrupt release a send blocked on the wire (see run_pull_transfer).
    let _wake_guard = register_shutdown_wake(reader);

    // Push: local side is Generator (sender); batch records outgoing data (is_sender=true).
    let batch_recording = batch_ctx
        .as_ref()
        .map(|ctx| build_batch_recording(ctx, true));

    let start = Instant::now();
    let mut adapter = observer.map(|obs| DaemonProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);

    // upstream: sender.c:640,775 log_item(FCLIENT) - on a push the client is the
    // sender, and the client-visible itemize row is printed by the SENDER from
    // the iflags the remote receiver's generator writes over the wire
    // (generator.c:593 write_shortint(sock_f_out, iflags) for protocol >=
    // 29). The remote generator's own log_item never reaches the client because
    // log.c:869-870 gates the FCLIENT write on `!am_server`. So the local sender is
    // the single source of push itemize; the remote receiver must not forward a
    // pre-rendered MSG_INFO line (see receiver::emit_itemize). This restores
    // output for oc-client -> upstream-daemon pushes, where upstream never
    // forwards oc's itemize.
    // upstream: sender.c:640 - plain `-v` (no `-i`) prints the bare `%n%L` name
    // per file too, so the callback must be wired whenever the sender has any
    // client-visible per-file output, not only under `-i`. A custom
    // `--out-format` also wants the rows even without `-v`/`-i` so the client can
    // render the template; the sink then collects metadata events instead of
    // writing the default line (mirrors the SSH push driver).
    let render_out_format = config.render_out_format_locally();
    // `-v` is not the gate: upstream logs names whenever NAME >= 1 sets
    // `stdout_format` (options.c:2522-2523), which `-P` does on its own.
    let wants_client_output = config.itemize_changes()
        || logging::info_gte(logging::InfoFlag::Name, 1)
        || render_out_format;
    let mut itemize_sink = ItemizeEventSink::new(render_out_format);

    let result = crate::server::run_server_with_handshake_adopting(
        server_config,
        handshake,
        reader,
        writer,
        crate::server::ServerTransferHooks {
            progress,
            batch: batch_recording,
            itemize: if wants_client_output {
                Some(&mut itemize_sink as &mut dyn crate::server::ItemizeCallback)
            } else {
                None
            },
            bytes_read: Some(Arc::clone(&bytes_read)),
            ..Default::default()
        },
    );

    match result {
        Ok(server_stats) => {
            let elapsed = start.elapsed();
            let mut summary = convert_server_stats_to_summary(server_stats, elapsed);
            let events = itemize_sink.take_events();
            if !events.is_empty() {
                summary = summary.with_events(events);
            }
            summary.set_protocol_version(protocol.as_u8());
            Ok(summary)
        }
        Err(ref e) if dry_run && is_dry_run_remote_close(e) => {
            // upstream: clientserver.c - during --dry-run push, the daemon closes
            // its socket early after receiving the file list.
            Ok(ClientSummary::default())
        }
        Err(e) => Err(map_server_transfer_error(
            e,
            ReportingSide::Sender,
            bytes_read.load(Ordering::Relaxed),
            io_timeout,
        )),
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
/// A stream that ends mid-transfer is upstream's end-of-stream whine, exit
/// `RERR_STREAMIO` (12), naming `bytes_received`. Other failures with no
/// embedded remote code (local I/O, protocol desync) keep the generic
/// `transfer failed: ...` (23) diagnostic.
///
/// upstream: io.c:1892-1925 - `MSG_ERROR_EXIT` drives the NORETURN
/// `_exit_cleanup(val)`, so the client's final exit code is the peer's code.
fn map_server_transfer_error(
    error: std::io::Error,
    side: ReportingSide,
    bytes_received: u64,
    io_timeout: Option<u64>,
) -> ClientError {
    // An interrupted transfer surfaces as whatever I/O failure the teardown
    // produced (a truncated multiplex frame, a closed socket). Upstream never
    // reports that: `rsync.c:843-874 sig_int()` short-circuits to
    // `exit_cleanup(RERR_SIGNAL)`, so the one diagnostic is the signal itself.
    if crate::signal::is_shutdown_requested() {
        return crate::client::error::signal_interrupt_error(side.local_role());
    }
    if let Some(code) = remote_exit_code(&error) {
        let exit = ExitCode::from_i32(code).unwrap_or(ExitCode::PartialTransfer);
        return remote_exit_error(exit, side.exit_role());
    }
    if let Some(error) = crate::client::error::corrupt_fake_super_destination_error(&error) {
        return error;
    }
    // The class mapping is the remote-shell transport's: a daemon socket has
    // no child status to weigh against the local code.
    local_failure_error(&error, None, side, bytes_received, io_timeout)
}

/// Walks the source chain of an `io::Error` looking for a
/// [`transfer::RemoteExitError`], returning the peer-supplied exit code when the
/// failure originated from a remote `MSG_ERROR_EXIT` frame.
fn remote_exit_code(error: &std::io::Error) -> Option<i32> {
    crate::server::remote_exit_code(error)
}

/// Builds a `HandshakeResult` for daemon transfers where the protocol version
/// was already negotiated via the `@RSYNCD` text exchange.
///
/// upstream: compat.c:602 - when `remote_protocol != 0`, `setup_protocol()`
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
/// - `main.c:1429` - `start_filesfrom_forwarding(filesfrom_fd)`
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
        let err = map_server_transfer_error(remote_exit_io(1), ReportingSide::Sender, 0, None);
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.code(), ExitCode::Syntax);
        let rendered = err.to_string();
        assert!(!rendered.contains("transfer failed"), "{rendered}");
        assert!(rendered.contains("[sender="), "{rendered}");
    }

    /// A pull rejected before the fork is reported by the lone `Receiver`
    /// (rsync.c:987-995 who_am_i()); after it, by the generator, the last
    /// process to exit.
    #[test]
    fn maps_remote_reject_pull_uses_the_phase_role() {
        let err = map_server_transfer_error(remote_exit_io(1), ReportingSide::PreFork, 0, None);
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("[Receiver="), "{err}");
        let side = ReportingSide::PostFork { generator_read: 79 };
        let err = map_server_transfer_error(remote_exit_io(1), side, 0, None);
        assert!(err.to_string().contains("[generator="), "{err}");
    }

    /// A receiver-side file-list validation rejection (implied-include or
    /// daemon-filter recheck, offset overflow) mirrors upstream
    /// `exit_cleanup(RERR_UNSUPPORTED)` (flist.c:1366,1370): the client exits
    /// 4, not the generic 23. Measured against upstream 3.5.0: pulling
    /// `mod/a\b*` from a daemon whose glob serves `ab.txt` prints "rejecting
    /// unrequested file-list name: ab.txt" and exits 4.
    #[test]
    fn maps_unsupported_rejection_to_rerr_unsupported() {
        let err = map_server_transfer_error(
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "ERROR: rejecting unrequested file-list name: ab.txt",
            ),
            ReportingSide::PreFork,
            0,
            None,
        );
        assert_eq!(err.exit_code(), 4);
        assert_eq!(err.code(), ExitCode::Unsupported);
    }

    /// A rule refused out of a per-directory merge file on a daemon push is
    /// upstream's filter_rule_err(): exit 1 and the bare text
    /// (exclude.c:133-137).
    #[test]
    fn maps_local_syntax_refusal_to_rerr_syntax_with_bare_text() {
        let err = map_server_transfer_error(
            protocol::syntax_violation("unexpected end of filter rule: <rule from /m/.f>"),
            ReportingSide::Sender,
            0,
            None,
        );
        assert_eq!(err.exit_code(), 1);
        assert!(!err.to_string().contains("transfer failed"), "{err}");
    }

    /// A failure with no embedded remote code exits by its class, as on the
    /// remote-shell transport: a broken socket is RERR_SOCKETIO, not a per-file
    /// partial transfer.
    #[test]
    fn maps_plain_failure_by_its_class() {
        let err = map_server_transfer_error(
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe"),
            ReportingSide::PreFork,
            0,
            None,
        );
        assert_eq!(err.exit_code(), 10);
        assert!(err.to_string().contains("[Receiver="), "{err}");
    }

    /// A local protocol violation exits RERR_PROTOCOL (2) like upstream, e.g.
    /// `exclude.c:1924-1926` refusing a filter rule a pre-29 peer cannot read.
    /// Reporting 23 would tell the user some files transferred.
    #[test]
    fn maps_local_protocol_violation_to_rerr_protocol() {
        let err = map_server_transfer_error(
            protocol::protocol_violation("filter rules are too modern for remote rsync."),
            ReportingSide::Sender,
            0,
            None,
        );
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("filter rules are too modern for remote rsync."),
            "{err}"
        );
    }

    /// A signal-driven teardown must report the signal, not the I/O failure it
    /// caused. Upstream emits exactly one line for an interrupt
    /// (`rsync.c:874` -> `log.c:log_exit()` with `RERR_SIGNAL`); reporting the
    /// truncated frame instead would show the user a protocol error and code
    /// 23 for something they asked for.
    #[test]
    fn signalled_teardown_reports_the_signal_not_the_io_failure() {
        crate::signal::reset_for_testing();
        crate::signal::request_shutdown(crate::signal::ShutdownReason::Terminated);
        let err = map_server_transfer_error(
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "multiplex truncated"),
            ReportingSide::PreFork,
            0,
            None,
        );
        crate::signal::reset_for_testing();
        assert_eq!(err.exit_code(), 20);
        let rendered = err.to_string();
        assert!(
            rendered.contains("received SIGINT, SIGTERM, or SIGHUP"),
            "{rendered}"
        );
        assert!(!rendered.contains("multiplex truncated"), "{rendered}");
    }

    /// A peer that closes the stream mid-transfer is upstream's end-of-stream
    /// whine, exit RERR_STREAMIO (12), naming the bytes received so far - not a
    /// per-file partial transfer (23). The rsync 3.5.1 strict-basis cell
    /// asserts exactly this for a sender that stops after its last token.
    /// upstream: io.c:282-304 whine_about_eof().
    #[test]
    fn maps_mid_transfer_eof_to_the_end_of_stream_whine() {
        let err = map_server_transfer_error(
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "multiplex truncated"),
            ReportingSide::PostFork { generator_read: 79 },
            112,
            None,
        );
        assert_eq!(err.exit_code(), 12);
        let rendered = err.to_string();
        assert!(
            rendered.contains("error in rsync protocol data stream"),
            "{rendered}"
        );
        assert_eq!(
            err.program_line(),
            Some("connection unexpectedly closed (79 bytes received so far) [generator]")
        );
        let receiver = err.preceding().expect("the receiver reports first");
        assert_eq!(
            receiver.program_line(),
            Some("connection unexpectedly closed (112 bytes received so far) [receiver]")
        );
    }

    /// upstream: io.c:259-266 check_timeout() - a daemon that stops sending
    /// for longer than --timeout ends the run with RERR_TIMEOUT (30). Measured
    /// against a 3.5.1 daemon stalled by `pre-xfer exec = sleep 5` under
    /// `--timeout=1`: "[Receiver] io timeout after 1 seconds -- exiting".
    #[test]
    fn maps_a_read_timeout_to_rerr_timeout() {
        let err = map_server_transfer_error(
            std::io::Error::from(std::io::ErrorKind::WouldBlock),
            ReportingSide::PreFork,
            0,
            Some(1),
        );
        assert_eq!(err.code(), ExitCode::Timeout);
        assert_eq!(
            err.cause_line(),
            Some("[Receiver] io timeout after 1 seconds -- exiting")
        );
        assert!(err.to_string().contains("[Receiver="), "{err}");
    }

    /// After the fork the receiver never times out; the generator does, and its
    /// exit SIGUSR1s the receiver, whose trailer prints last (io.c:256-266,
    /// cleanup.c:202-203). Measured against a 3.5.1 client fed a stalled
    /// replayed stream after its file list, 12 runs of 12: the generator's
    /// line and code-30 trailer, then `received SIGUSR1 (code 19) [receiver=`.
    #[test]
    fn a_timeout_after_the_fork_is_the_generators() {
        let err = map_server_transfer_error(
            std::io::Error::from(std::io::ErrorKind::WouldBlock),
            ReportingSide::PostFork { generator_read: 26 },
            26,
            Some(1),
        );
        assert_eq!(err.code(), ExitCode::Timeout);
        let rendered = err.to_string();
        assert!(rendered.contains("(code 19)"), "{rendered}");
        assert!(rendered.contains("[receiver="), "{rendered}");
        let generator = err.preceding().expect("the generator reports first");
        assert_eq!(
            generator.cause_line(),
            Some("[generator] io timeout after 1 seconds -- exiting")
        );
        assert_eq!(generator.code(), ExitCode::Timeout);
        assert!(generator.to_string().contains("[generator="), "{generator}");
    }

    /// Without a --timeout a would-block read is no timeout: it keeps its
    /// file-I/O class rather than claiming RERR_TIMEOUT.
    #[test]
    fn a_would_block_read_without_a_timeout_is_not_a_timeout() {
        let err = map_server_transfer_error(
            std::io::Error::from(std::io::ErrorKind::WouldBlock),
            ReportingSide::PreFork,
            0,
            None,
        );
        assert_ne!(err.code(), ExitCode::Timeout);
        assert!(err.cause_line().is_none());
    }

    /// upstream: io.c:930-946 perform_io() - a reset socket after the fork is
    /// the receiver's `read error` (RERR_SOCKETIO), then the generator's
    /// end-of-file whine and RERR_STREAMIO. Measured against a 3.5.1 daemon
    /// whose stream a proxy resets after 2000 bytes: rc 12.
    #[test]
    fn a_reset_socket_after_the_fork_ends_on_the_generator_whine() {
        let err = map_server_transfer_error(
            std::io::Error::from_raw_os_error(libc_econnreset()),
            ReportingSide::PostFork { generator_read: 79 },
            1905,
            None,
        );
        assert_eq!(err.code(), ExitCode::StreamIo);
        let receiver = err.preceding().expect("the receiver reports first");
        assert_eq!(receiver.code(), ExitCode::SocketIo);
        let line = receiver.program_line().expect("read error line");
        assert!(line.starts_with("[receiver] read error: "), "{line}");
        assert!(receiver.to_string().contains("[receiver="), "{receiver}");
    }

    /// `ECONNRESET` (`WSAECONNRESET` on Windows) as the platform numbers it.
    const fn libc_econnreset() -> i32 {
        if cfg!(windows) {
            10054
        } else if cfg!(target_os = "linux") {
            104
        } else {
            54
        }
    }
}
