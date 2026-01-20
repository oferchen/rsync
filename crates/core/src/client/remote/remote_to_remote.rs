//! Remote-to-remote transfer via local proxy.
//!
//! This module implements transfers between two remote hosts, with the local
//! machine acting as a relay/proxy. The local process spawns two SSH connections:
//!
//! 1. To the source host with `rsync --server --sender` (generator role)
//! 2. To the destination host with `rsync --server` (receiver role)
//!
//! Protocol messages are relayed bidirectionally between the two connections.
//!
//! # Data Flow
//!
//! ```text
//! Source Host                  Local (Proxy)                 Dest Host
//! ┌─────────────────┐         ┌─────────────────┐         ┌─────────────────┐
//! │ rsync --server  │   SSH   │                 │   SSH   │ rsync --server  │
//! │ --sender        │◄───────►│  Bidirectional  │◄───────►│ (receiver)      │
//! │ (generator)     │         │  Relay          │         │                 │
//! └─────────────────┘         └─────────────────┘         └─────────────────┘
//! ```
//!
//! # Implementation Notes
//!
//! - Uses two threads for deadlock-free bidirectional relay
//! - One thread copies source → destination
//! - Another thread copies destination → source
//! - Both threads run until EOF or error

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rsync_io::ssh::{SshCommand, SshConnection, SshReader, SshWriter, parse_ssh_operand};

use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error};
use super::super::summary::ClientSummary;
use super::invocation::{RemoteInvocationBuilder, RemoteOperands, RemoteRole};

/// Statistics from a proxy transfer.
#[derive(Debug, Default)]
struct ProxyStats {
    /// Bytes relayed from source to destination.
    bytes_source_to_dest: u64,
    /// Bytes relayed from destination to source.
    bytes_dest_to_source: u64,
}

/// Executes a remote-to-remote transfer via local proxy.
///
/// Spawns two SSH connections and relays protocol messages between them.
///
/// # Arguments
///
/// * `config` - Client configuration with transfer options
/// * `remote_sources` - Remote source operand(s)
/// * `remote_dest` - Remote destination operand
///
/// # Returns
///
/// A client summary with transfer statistics on success.
pub fn run_remote_to_remote_transfer(
    config: &ClientConfig,
    remote_sources: RemoteOperands,
    remote_dest: String,
) -> Result<ClientSummary, ClientError> {
    // Parse remote operands
    let (source_host, source_user, source_port, source_paths) =
        parse_source_operands(&remote_sources)?;
    let (dest_host, dest_user, dest_port, dest_path) = parse_dest_operand(&remote_dest)?;

    // Build invocation arguments for both sides
    let source_invocation = build_source_invocation(config, &source_paths);
    let dest_invocation = build_dest_invocation(config, &dest_path);

    // Spawn SSH connections
    let source_conn = spawn_ssh_connection(
        &source_user,
        &source_host,
        source_port,
        &source_invocation,
        config,
    )?;
    let dest_conn =
        spawn_ssh_connection(&dest_user, &dest_host, dest_port, &dest_invocation, config)?;

    // Run bidirectional relay
    let stats = run_bidirectional_relay(source_conn, dest_conn)?;

    // Convert stats to client summary
    Ok(build_proxy_summary(stats))
}

/// Parses source operand(s) and extracts connection info.
#[allow(clippy::type_complexity)]
fn parse_source_operands(
    operands: &RemoteOperands,
) -> Result<(String, Option<String>, Option<u16>, Vec<String>), ClientError> {
    match operands {
        RemoteOperands::Single(operand_str) => {
            let operand = parse_ssh_operand(OsStr::new(operand_str))
                .map_err(|e| invalid_argument_error(&format!("invalid source operand: {e}"), 1))?;
            Ok((
                operand.host().to_owned(),
                operand.user().map(String::from),
                operand.port(),
                vec![operand.path().to_owned()],
            ))
        }
        RemoteOperands::Multiple(operand_strs) => {
            let first = parse_ssh_operand(OsStr::new(&operand_strs[0]))
                .map_err(|e| invalid_argument_error(&format!("invalid source operand: {e}"), 1))?;

            let mut paths = Vec::with_capacity(operand_strs.len());
            for operand_str in operand_strs {
                let operand = parse_ssh_operand(OsStr::new(operand_str)).map_err(|e| {
                    invalid_argument_error(&format!("invalid source operand: {e}"), 1)
                })?;
                paths.push(operand.path().to_owned());
            }

            Ok((
                first.host().to_owned(),
                first.user().map(String::from),
                first.port(),
                paths,
            ))
        }
    }
}

/// Parses the destination operand and extracts connection info.
fn parse_dest_operand(
    operand_str: &str,
) -> Result<(String, Option<String>, Option<u16>, String), ClientError> {
    let operand = parse_ssh_operand(OsStr::new(operand_str))
        .map_err(|e| invalid_argument_error(&format!("invalid destination operand: {e}"), 1))?;
    Ok((
        operand.host().to_owned(),
        operand.user().map(String::from),
        operand.port(),
        operand.path().to_owned(),
    ))
}

/// Builds the rsync invocation for the source (sender) side.
fn build_source_invocation(config: &ClientConfig, paths: &[String]) -> Vec<OsString> {
    let builder = RemoteInvocationBuilder::new(config, RemoteRole::Sender);
    if paths.len() == 1 {
        builder.build(&paths[0])
    } else {
        let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        builder.build_with_paths(&path_refs)
    }
}

/// Builds the rsync invocation for the destination (receiver) side.
fn build_dest_invocation(config: &ClientConfig, path: &str) -> Vec<OsString> {
    let builder = RemoteInvocationBuilder::new(config, RemoteRole::Receiver);
    builder.build(path)
}

/// Spawns an SSH connection to a remote host.
fn spawn_ssh_connection(
    user: &Option<String>,
    host: &str,
    port: Option<u16>,
    invocation_args: &[OsString],
    config: &ClientConfig,
) -> Result<SshConnection, ClientError> {
    let mut ssh = SshCommand::new(host);

    if let Some(user) = user {
        ssh.set_user(user);
    }

    if let Some(port) = port {
        ssh.set_port(port);
    }

    // Configure custom remote shell if specified
    if let Some(shell_args) = config.remote_shell()
        && !shell_args.is_empty()
    {
        ssh.set_program(&shell_args[0]);
        for arg in &shell_args[1..] {
            ssh.push_option(arg.clone());
        }
    }

    ssh.set_remote_command(invocation_args);

    ssh.spawn().map_err(|e| {
        invalid_argument_error(
            &format!("failed to spawn SSH connection to {host}: {e}"),
            super::super::IPC_EXIT_CODE,
        )
    })
}

/// Timeout for waiting on relay threads to complete after the transfer is done.
const RELAY_THREAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs bidirectional relay between two SSH connections using threads.
///
/// Spawns two threads for deadlock-free bidirectional relay:
/// - Thread 1: source stdout → destination stdin (file data, protocol messages)
/// - Thread 2: destination stdout → source stdin (checksums, acks)
///
/// Both threads run concurrently until EOF or error. This mirrors upstream rsync's
/// approach of using non-blocking I/O for proxy transfers.
///
/// # Arguments
///
/// * `source` - SSH connection to the source host (sender/generator)
/// * `dest` - SSH connection to the destination host (receiver)
///
/// # Returns
///
/// Statistics on bytes relayed in each direction on success.
fn run_bidirectional_relay(
    source: SshConnection,
    dest: SshConnection,
) -> Result<ProxyStats, ClientError> {
    // Split connections into read/write halves for thread-safe operation
    let (source_reader, source_writer, source_handle) = source.split().map_err(|e| {
        invalid_argument_error(&format!("failed to split source connection: {e}"), 23)
    })?;
    let (dest_reader, dest_writer, dest_handle) = dest.split().map_err(|e| {
        invalid_argument_error(&format!("failed to split destination connection: {e}"), 23)
    })?;

    // Shared flag to signal shutdown
    let shutdown = Arc::new(AtomicBool::new(false));

    // Channels for receiving thread completion (enables timeout)
    let (s2d_tx, s2d_rx) = mpsc::channel();
    let (d2s_tx, d2s_rx) = mpsc::channel();

    // Thread 1: source → destination (file data, file list)
    let shutdown_s2d = Arc::clone(&shutdown);
    thread::spawn(move || {
        let result =
            run_relay_with_panic_guard(source_reader, dest_writer, shutdown_s2d, "source→dest");
        let _ = s2d_tx.send(result);
    });

    // Thread 2: destination → source (checksums, acks)
    let shutdown_d2s = Arc::clone(&shutdown);
    thread::spawn(move || {
        let result =
            run_relay_with_panic_guard(dest_reader, source_writer, shutdown_d2s, "dest→source");
        let _ = d2s_tx.send(result);
    });

    // Wait for both threads with timeout
    let s2d_result = s2d_rx
        .recv_timeout(RELAY_THREAD_TIMEOUT)
        .map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => {
                shutdown.store(true, Ordering::SeqCst);
                invalid_argument_error("source→dest relay thread timed out", 23)
            }
            mpsc::RecvTimeoutError::Disconnected => {
                invalid_argument_error("source→dest relay thread terminated unexpectedly", 23)
            }
        })??;

    let d2s_result = d2s_rx
        .recv_timeout(RELAY_THREAD_TIMEOUT)
        .map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => {
                shutdown.store(true, Ordering::SeqCst);
                invalid_argument_error("dest→source relay thread timed out", 23)
            }
            mpsc::RecvTimeoutError::Disconnected => {
                invalid_argument_error("dest→source relay thread terminated unexpectedly", 23)
            }
        })??;

    // Wait for child processes
    let _ = source_handle.wait();
    let _ = dest_handle.wait();

    Ok(ProxyStats {
        bytes_source_to_dest: s2d_result,
        bytes_dest_to_source: d2s_result,
    })
}

/// Runs the relay with panic recovery using `catch_unwind`.
///
/// Wraps `relay_data` in panic handling to ensure the shutdown flag is set
/// on panic and to capture panic information for better error messages.
fn run_relay_with_panic_guard(
    reader: SshReader,
    writer: SshWriter,
    shutdown: Arc<AtomicBool>,
    direction: &'static str,
) -> Result<u64, ClientError> {
    let shutdown_clone = Arc::clone(&shutdown);
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        relay_data(reader, writer, shutdown, direction)
    }));

    match result {
        Ok(inner_result) => inner_result,
        Err(panic_info) => {
            // Ensure shutdown is signaled on panic
            shutdown_clone.store(true, Ordering::SeqCst);
            let message = if let Some(s) = panic_info.downcast_ref::<&str>() {
                format!("{direction} relay thread panicked: {s}")
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                format!("{direction} relay thread panicked: {s}")
            } else {
                format!("{direction} relay thread panicked")
            };
            Err(invalid_argument_error(&message, 23))
        }
    }
}

/// Relays data from reader to writer until EOF or error.
///
/// Copies data in chunks, flushing after each write to maintain protocol
/// synchronization. Returns the total bytes relayed.
///
/// # Arguments
///
/// * `reader` - Source of data
/// * `writer` - Destination for data
/// * `shutdown` - Shared flag to check for shutdown request
/// * `direction` - Human-readable direction for error messages
fn relay_data(
    mut reader: SshReader,
    mut writer: SshWriter,
    shutdown: Arc<AtomicBool>,
    direction: &str,
) -> Result<u64, ClientError> {
    let mut buf = [0u8; 8192];
    let mut total_bytes = 0u64;

    loop {
        // Check for shutdown signal
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match reader.read(&mut buf) {
            Ok(0) => {
                // EOF reached - signal shutdown and close writer
                shutdown.store(true, Ordering::Relaxed);
                let _ = writer.close();
                break;
            }
            Ok(n) => {
                writer.write_all(&buf[..n]).map_err(|e| {
                    shutdown.store(true, Ordering::Relaxed);
                    invalid_argument_error(&format!("{direction} write error: {e}"), 23)
                })?;
                writer.flush().map_err(|e| {
                    shutdown.store(true, Ordering::Relaxed);
                    invalid_argument_error(&format!("{direction} flush error: {e}"), 23)
                })?;
                total_bytes += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Non-blocking I/O - yield and retry
                thread::yield_now();
                continue;
            }
            Err(e) => {
                shutdown.store(true, Ordering::Relaxed);
                return Err(invalid_argument_error(
                    &format!("{direction} read error: {e}"),
                    23,
                ));
            }
        }
    }

    Ok(total_bytes)
}

/// Builds a client summary from proxy transfer statistics.
fn build_proxy_summary(stats: ProxyStats) -> ClientSummary {
    use engine::local_copy::LocalCopySummary;

    // Create a summary with the bytes relayed
    let summary =
        LocalCopySummary::from_proxy_stats(stats.bytes_source_to_dest, stats.bytes_dest_to_source);

    ClientSummary::from_summary(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_source_operand() {
        let operands = RemoteOperands::Single("user@host:/path/to/file".to_string());
        let (host, user, port, paths) = parse_source_operands(&operands).unwrap();
        assert_eq!(host, "host");
        assert_eq!(user, Some("user".to_string()));
        assert_eq!(port, None);
        assert_eq!(paths, vec!["/path/to/file"]);
    }

    #[test]
    fn parse_multiple_source_operands() {
        let operands = RemoteOperands::Multiple(vec![
            "user@host:/path/one".to_string(),
            "user@host:/path/two".to_string(),
        ]);
        let (host, user, port, paths) = parse_source_operands(&operands).unwrap();
        assert_eq!(host, "host");
        assert_eq!(user, Some("user".to_string()));
        assert_eq!(port, None);
        assert_eq!(paths, vec!["/path/one", "/path/two"]);
    }

    #[test]
    fn parse_dest_operand_basic() {
        let (host, user, port, path) = parse_dest_operand("root@server:/backup").unwrap();
        assert_eq!(host, "server");
        assert_eq!(user, Some("root".to_string()));
        assert_eq!(port, None);
        assert_eq!(path, "/backup");
    }
}
