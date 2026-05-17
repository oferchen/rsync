//! Async SSH transfer wiring built on `rsync_io::ssh::AsyncSshTransport`.
//!
//! Bridges the tokio-backed SSH transport (task #1796) into the existing
//! synchronous client transfer orchestration. The synchronous handshake
//! and server-side framing layer is preserved verbatim; only the byte
//! transport is moved onto `tokio::process` plus the `ChannelReader` /
//! `ChannelWriter` adapters from task #1797. This is the wiring milestone
//! for #1805. The CLI surface that lets users opt in by flag is deferred
//! to #1806; today the path is selected by setting the
//! `OC_RSYNC_ASYNC_SSH=1` environment variable before invoking the
//! client.
//!
//! # Architecture
//!
//! ```text
//!  spawn_blocking thread          tokio runtime (current_thread)
//!  ─────────────────────          ──────────────────────────────
//!  SyncWriter ──┐                                         ┌── AsyncSshTransport
//!               │  std::sync::mpsc  ChannelReader (Async) │
//!               └─────────────────► (pump)  ─────────────►│  stdin (AsyncWrite)
//!                                                         │
//!  SyncReader ◄─┐                                         │  stdout (AsyncRead)
//!               │  std::sync::mpsc  ChannelWriter (Async) │
//!               └◄──────────────── (pump)  ◄──────────────│
//! ```
//!
//! Two `std::sync::mpsc` channels carry byte chunks between the sync
//! server thread (running under `tokio::task::spawn_blocking`) and the
//! async pump tasks. Each pump task uses the `ChannelReader` /
//! `ChannelWriter` adapters from PR #4271 to interoperate with the
//! `AsyncRead` / `AsyncWrite` halves exposed by
//! [`AsyncSshTransport::split`]. The synchronous handshake and
//! server-side framing layer is reused unchanged from the system SSH
//! path.
//!
//! # Feature gate
//!
//! Compiled only when the `async-ssh` feature is enabled (which itself
//! pulls in `rsync_io/async-ssh` and `dep:tokio`). Default builds remain
//! on the synchronous transport.

use std::ffi::{OsStr, OsString};
use std::io;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::batch::BatchWriter;
use rsync_io::channel_adapter::{ChannelReader, ChannelWriter};
use rsync_io::ssh::{AsyncSshTransport, SshConnectConfig, parse_ssh_operand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Builder;
use tokio::sync::mpsc as tokio_mpsc;

use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error, invalid_argument_error_typed};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::batch_support::{BatchContext, build_batch_context, build_batch_recording};
use super::flags;
use super::invocation::{RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role};
use super::ssh_transfer::{
    build_server_config_for_generator, build_server_config_for_receiver,
    convert_server_stats_to_summary, parse_remote_operands, parse_single_remote,
};
use crate::exit_code::ExitCode;
use crate::server::{ServerConfig, ServerRole, ServerStats};

/// Default capacity for the in-process byte chunk channels that connect
/// the sync server thread to the async pump tasks. Each chunk is one
/// `Vec<u8>` produced by a single `write()` or `read()` call. 32 keeps
/// the reader and writer modestly decoupled without ballooning peak
/// memory.
const CHANNEL_CAPACITY: usize = 32;

/// Buffer size used by the async pumps when copying between
/// `AsyncRead`/`AsyncWrite` and the in-process channels. Matches the
/// upstream-compatible 32 KiB chunk used by the sync transport.
const PUMP_BUF: usize = 32 * 1024;

/// Environment variable that opts the client into the async SSH
/// transport.
///
/// Setting `OC_RSYNC_ASYNC_SSH=1` before invoking the client driver
/// routes SSH transfers through [`run_async_ssh_transfer`]. Any other
/// value (or the variable being unset) falls back to the synchronous
/// path. The toggle is intentional scaffolding until task #1806 adds a
/// CLI surface.
pub const ENV_OPT_IN: &str = "OC_RSYNC_ASYNC_SSH";

/// Returns `true` when the env var opt-in is active.
#[must_use]
pub fn is_enabled_by_env() -> bool {
    std::env::var(ENV_OPT_IN)
        .map(|v| is_truthy_env_value(&v))
        .unwrap_or(false)
}

/// Pure parser for the env-var opt-in toggle, factored out so unit
/// tests can verify the matching without mutating the process-wide
/// environment.
fn is_truthy_env_value(value: &str) -> bool {
    matches!(value, "1" | "true" | "yes" | "on")
}

/// Executes a transfer over the async SSH transport.
///
/// Entry point analogous to `run_ssh_transfer` but with the byte
/// transport running on `tokio::process` via [`AsyncSshTransport`]. The
/// synchronous handshake and server-side framing layer is reused
/// unchanged; only the stdio pipes are owned by the tokio runtime, with
/// byte chunks flowing through `std::sync::mpsc` bridges to the blocking
/// server thread.
///
/// Progress observation and remote-to-remote proxying are intentionally
/// not wired on this path yet; both rely on lifetime gymnastics that
/// land alongside the CLI flag in #1806.
///
/// # Errors
///
/// Returns an error for the same conditions as `run_ssh_transfer`:
/// operand parsing failure, SSH spawn failure, handshake failure, or
/// transfer failure.
pub fn run_async_ssh_transfer(
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

    match transfer_spec {
        TransferSpec::Push {
            local_sources,
            remote_dest,
        } => {
            let plan = build_plan(config, RemoteRole::Sender, &remote_dest, None)?;
            let server_config = build_push_server_config(config, &local_sources)?;
            run_async_session(config, plan, server_config, batch_writer)
        }
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            let plan = build_plan(config, RemoteRole::Receiver, "", Some(&remote_sources))?;
            let server_config = build_pull_server_config(config, &[local_dest])?;
            run_async_session(config, plan, server_config, batch_writer)
        }
        TransferSpec::Proxy { .. } => Err(invalid_argument_error(
            "async SSH transport does not yet support remote-to-remote proxy transfers (#1805)",
            1,
        )),
    }
}

/// Resolved spawn details for a single-endpoint async SSH session.
struct AsyncSpawnPlan {
    remote: String,
    invocation_args: Vec<OsString>,
    stdin_args: Vec<String>,
    config: SshConnectConfig,
}

fn build_plan(
    config: &ClientConfig,
    role: RemoteRole,
    single_dest: &str,
    pull_sources: Option<&RemoteOperands>,
) -> Result<AsyncSpawnPlan, ClientError> {
    let (invocation_args, host, user, _port, stdin_args) = if let Some(sources) = pull_sources {
        parse_remote_operands(sources, config, role)?
    } else {
        parse_single_remote(single_dest, config, role)?
    };

    let remote = match user {
        Some(user) => format!("{user}@{host}"),
        None => host,
    };

    let connect_timeout = config.connect_timeout().effective(Duration::from_secs(30));
    let connect_config = SshConnectConfig::new().with_connect_timeout(connect_timeout);

    Ok(AsyncSpawnPlan {
        remote,
        invocation_args,
        stdin_args,
        config: connect_config,
    })
}

fn build_pull_server_config(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let mut server_config = build_server_config_for_receiver(config, local_paths)?;
    server_config.connection.client_mode = true;
    server_config.connection.filter_rules = flags::build_wire_format_rules(config.filter_rules())
        .map_err(|e| {
        invalid_argument_error(&format!("failed to build filter rules: {e}"), 12)
    })?;
    server_config.stop_at = config.stop_at();
    Ok(server_config)
}

fn build_push_server_config(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let mut server_config = build_server_config_for_generator(config, local_paths)?;
    server_config.connection.client_mode = true;
    server_config.connection.filter_rules = flags::build_wire_format_rules(config.filter_rules())
        .map_err(|e| {
        invalid_argument_error(&format!("failed to build filter rules: {e}"), 12)
    })?;
    server_config.stop_at = config.stop_at();
    Ok(server_config)
}

/// Drives a single async SSH session end to end.
///
/// Builds a current-thread tokio runtime, spawns
/// [`AsyncSshTransport::execute_remote_rsync`], wires the read/write
/// halves through `ChannelReader` / `ChannelWriter` bridges to a pair of
/// sync `std::sync::mpsc` channels, runs the existing sync server
/// transfer loop on a `spawn_blocking` worker, then joins the pumps
/// before returning. The sync transfer result is mapped through the
/// same error model the system SSH path uses.
fn run_async_session(
    client_config: &ClientConfig,
    plan: AsyncSpawnPlan,
    server_config: ServerConfig,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| invalid_argument_error(&format!("failed to build tokio runtime: {e}"), 12))?;

    let stdin_args = plan.stdin_args.clone();
    let iconv_converter = if client_config.protect_args().unwrap_or(false) {
        client_config.iconv().resolve_converter()
    } else {
        None
    };

    let batch_ctx = batch_writer.map(|bw| build_batch_context(client_config, bw));

    runtime.block_on(async move {
        let transport = AsyncSshTransport::execute_remote_rsync(
            &plan.remote,
            &plan.invocation_args,
            &plan.config,
        )
        .await
        .map_err(|e| {
            invalid_argument_error(
                &format!("failed to spawn async SSH connection: {e}"),
                super::super::IPC_EXIT_CODE,
            )
        })?;

        let (mut async_reader, mut async_writer) = transport_split(transport);

        // upstream: rsync.c:283-320 send_protected_args() ships the
        // null-separated arg list over stdin before protocol negotiation.
        // Done here on the async side because AsyncSshTransport owns
        // stdin until we hand it to the pump below.
        if !stdin_args.is_empty() {
            let arg_refs: Vec<&str> = stdin_args.iter().map(String::as_str).collect();
            protocol::cmd::trace_protected_args(&arg_refs);
            let payload = encode_secluded_args(&arg_refs, iconv_converter.as_ref());
            async_writer.write_all(&payload).await.map_err(|e| {
                invalid_argument_error(
                    &format!("failed to send secluded args: {e}"),
                    super::super::IPC_EXIT_CODE,
                )
            })?;
            async_writer.flush().await.map_err(|e| {
                invalid_argument_error(
                    &format!("failed to flush secluded args: {e}"),
                    super::super::IPC_EXIT_CODE,
                )
            })?;
        }

        // Async-side channels driving the ChannelReader/Writer adapters.
        let (sync_to_ssh_tx, sync_to_ssh_rx) = tokio_mpsc::channel::<Vec<u8>>(CHANNEL_CAPACITY);
        let (ssh_to_sync_tx, ssh_to_sync_rx) = tokio_mpsc::channel::<Vec<u8>>(CHANNEL_CAPACITY);

        let mut writer_bridge = ChannelReader::new(sync_to_ssh_rx);
        let mut reader_bridge = ChannelWriter::new(ssh_to_sync_tx);

        // Outbound pump: drain sync writes from the channel and push them
        // into the ssh stdin half.
        let outbound = tokio::spawn(async move {
            let mut buf = vec![0u8; PUMP_BUF];
            loop {
                let n = writer_bridge.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                async_writer.write_all(&buf[..n]).await?;
            }
            async_writer.shutdown().await?;
            Ok::<(), io::Error>(())
        });

        // Inbound pump: read ssh stdout and forward bytes to the sync
        // reader via ChannelWriter.
        let inbound = tokio::spawn(async move {
            let mut buf = vec![0u8; PUMP_BUF];
            loop {
                let n = async_reader.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                reader_bridge.write_all(&buf[..n]).await?;
            }
            reader_bridge.shutdown().await?;
            Ok::<(), io::Error>(())
        });

        // Sync side of the bridge: SyncReader/SyncWriter that the
        // blocking server thread sees, fed by std_mpsc channels which are
        // in turn pumped to/from the tokio mpsc channels above.
        let (sync_reader_tx, sync_reader_rx) = std_mpsc::sync_channel::<Vec<u8>>(CHANNEL_CAPACITY);
        let (sync_writer_tx, sync_writer_rx) = std_mpsc::sync_channel::<Vec<u8>>(CHANNEL_CAPACITY);

        let mut ssh_to_sync_rx = ssh_to_sync_rx;
        let reader_fanout = tokio::spawn(async move {
            while let Some(chunk) = ssh_to_sync_rx.recv().await {
                if sync_reader_tx.send(chunk).is_err() {
                    break;
                }
            }
        });

        let writer_fanin = tokio::task::spawn_blocking(move || {
            while let Ok(chunk) = sync_writer_rx.recv() {
                if sync_to_ssh_tx.blocking_send(chunk).is_err() {
                    break;
                }
            }
        });

        let sync_reader = SyncReader::new(sync_reader_rx);
        let sync_writer = SyncWriter::new(sync_writer_tx);

        let batch_ctx_for_blocking = batch_ctx;
        let server_handle = tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            run_blocking_server(
                server_config,
                sync_reader,
                sync_writer,
                batch_ctx_for_blocking,
                start,
            )
        });

        let server_outcome = server_handle.await.map_err(|e| {
            invalid_argument_error(
                &format!("async SSH server task panicked: {e}"),
                ExitCode::WaitChild.as_i32(),
            )
        })?;

        // Drain background tasks so nothing outlives this future. Pump
        // errors are advisory; the authoritative result is the server
        // outcome.
        let _ = reader_fanout.await;
        let _ = writer_fanin.await;
        let _ = outbound.await;
        let _ = inbound.await;

        match server_outcome {
            Ok((stats, elapsed)) => Ok(convert_server_stats_to_summary(stats, elapsed)),
            Err(err) => Err(err),
        }
    })
}

/// Splits the transport without exposing tokio types in our public API.
fn transport_split(
    transport: AsyncSshTransport,
) -> (
    Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
) {
    let (read_half, write_half) = transport.split();
    (Box::new(read_half), Box::new(write_half))
}

/// Encodes the null-separated secluded-args payload that upstream rsync
/// pushes to the remote process over stdin. Mirrors
/// `protocol::secluded_args::send_secluded_args` but returns the bytes
/// instead of writing them, so the async pump can ship the buffer with
/// `AsyncWriteExt::write_all`.
///
/// upstream: rsync.c:283-320 send_protected_args() / iconvbufs(ic_send).
fn encode_secluded_args(
    args: &[&str],
    iconv: Option<&protocol::iconv::FilenameConverter>,
) -> Vec<u8> {
    let mut payload = Vec::new();
    for arg in args {
        match iconv {
            Some(converter) => match converter.local_to_remote(arg.as_bytes()) {
                Ok(bytes) => payload.extend_from_slice(&bytes),
                Err(_) => payload.extend_from_slice(arg.as_bytes()),
            },
            None => payload.extend_from_slice(arg.as_bytes()),
        }
        payload.push(0);
    }
    payload.push(0);
    payload
}

/// Sync `Read` adapter over a `std::sync::mpsc::Receiver<Vec<u8>>`.
///
/// Mirrors the semantics of the async [`ChannelReader`]: oversized
/// chunks are retained internally and drained across multiple reads, and
/// a closed sender surfaces as EOF (`Ok(0)`).
struct SyncReader {
    rx: std_mpsc::Receiver<Vec<u8>>,
    buffered: Vec<u8>,
    offset: usize,
}

impl SyncReader {
    fn new(rx: std_mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffered: Vec::new(),
            offset: 0,
        }
    }
}

impl std::io::Read for SyncReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.offset >= self.buffered.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        return Ok(0);
                    }
                    self.buffered = chunk;
                    self.offset = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let remaining = &self.buffered[self.offset..];
        let n = remaining.len().min(buf.len());
        buf[..n].copy_from_slice(&remaining[..n]);
        self.offset += n;
        if self.offset >= self.buffered.len() {
            self.buffered.clear();
            self.offset = 0;
        }
        Ok(n)
    }
}

/// Sync `Write` adapter over a `std::sync::mpsc::SyncSender<Vec<u8>>`.
///
/// Each `write` call ships a single chunk on the channel, matching how
/// the async [`ChannelWriter`] forwards every `poll_write` as one
/// message. A closed receiver surfaces as `BrokenPipe`.
struct SyncWriter {
    tx: Option<std_mpsc::SyncSender<Vec<u8>>>,
}

impl SyncWriter {
    fn new(tx: std_mpsc::SyncSender<Vec<u8>>) -> Self {
        Self { tx: Some(tx) }
    }
}

impl std::io::Write for SyncWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "writer shut down"))?;
        tx.send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "channel closed"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for SyncWriter {
    fn drop(&mut self) {
        // Dropping the sender signals EOF to the outbound pump, which
        // then shuts down the ssh stdin half.
        self.tx = None;
    }
}

/// Runs the sync server flow against the bridged Read/Write pair.
fn run_blocking_server(
    config: ServerConfig,
    mut reader: SyncReader,
    mut writer: SyncWriter,
    batch_ctx: Option<BatchContext>,
    start: Instant,
) -> Result<(ServerStats, Duration), ClientError> {
    let batch_recording = batch_ctx.as_ref().map(|ctx| {
        let is_sender = config.role == ServerRole::Generator;
        build_batch_recording(ctx, is_sender)
    });

    let handshake = crate::server::perform_handshake(&mut reader, &mut writer)
        .map_err(|e| invalid_argument_error(&format!("async SSH handshake failed: {e}"), 5))?;

    let transfer_result = crate::server::run_server_with_handshake(
        config,
        handshake,
        &mut reader,
        &mut writer,
        None,
        batch_recording,
        None,
    );

    // Drop the writer to signal EOF so the remote process can exit
    // cleanly.
    drop(writer);

    match transfer_result {
        Ok(stats) => Ok((stats, start.elapsed())),
        Err(err) => {
            let exit = ExitCode::from_io_error(&err);
            Err(invalid_argument_error_typed(
                &format!("async SSH transfer failed: {err}"),
                exit,
            ))
        }
    }
}

/// Returns `true` when the given operand string designates an SSH-style
/// remote. Wrapper around [`parse_ssh_operand`] used in tests to confirm
/// the same operand vocabulary the sync path accepts is recognised here.
#[allow(dead_code)] // REASON: reserved for upcoming dispatch helpers.
fn looks_like_ssh_operand(operand: &str) -> bool {
    parse_ssh_operand(OsStr::new(operand)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn sync_reader_drains_chunks_across_reads() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(4);
        tx.send(b"abcdef".to_vec()).unwrap();
        drop(tx);

        let mut reader = SyncReader::new(rx);
        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf[..n], b"abc");

        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"def");
    }

    #[test]
    fn sync_reader_closed_channel_is_eof() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(1);
        drop(tx);
        let mut reader = SyncReader::new(rx);
        let mut buf = [0u8; 4];
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn sync_writer_round_trips_chunks() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(2);
        let mut writer = SyncWriter::new(tx);
        writer.write_all(b"hello").unwrap();
        writer.write_all(b"world").unwrap();
        drop(writer);

        let mut collected = Vec::new();
        while let Ok(chunk) = rx.recv() {
            collected.extend_from_slice(&chunk);
        }
        assert_eq!(collected, b"helloworld");
    }

    #[test]
    fn sync_writer_to_closed_channel_is_broken_pipe() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(1);
        drop(rx);
        let mut writer = SyncWriter::new(tx);
        let err = writer.write_all(b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn env_opt_in_recognises_truthy_values() {
        for v in ["1", "true", "yes", "on"] {
            assert!(is_truthy_env_value(v), "expected truthy for {v}");
        }
        for v in ["0", "false", "off", "no", "", "True", "YES", "ON"] {
            assert!(!is_truthy_env_value(v), "expected falsey for {v:?}");
        }
    }

    #[test]
    fn looks_like_ssh_operand_accepts_user_host_path() {
        assert!(looks_like_ssh_operand("backup@example.com:/data"));
        assert!(looks_like_ssh_operand("example.com:/data"));
        assert!(!looks_like_ssh_operand("/local/path"));
    }

    #[test]
    fn encode_secluded_args_emits_null_terminated_list() {
        let payload = encode_secluded_args(&["rsync", "--server", "."], None);
        let expected: Vec<u8> = b"rsync\0--server\0.\0\0".to_vec();
        assert_eq!(payload, expected);
    }

    #[test]
    fn encode_secluded_args_empty_only_terminator() {
        let payload = encode_secluded_args(&[], None);
        assert_eq!(payload, vec![0u8]);
    }
}
