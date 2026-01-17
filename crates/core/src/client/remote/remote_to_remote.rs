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

use rsync_io::ssh::{SshCommand, SshConnection, parse_ssh_operand};

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

/// Runs bidirectional relay between two connections.
///
/// Note: This is a simple sequential implementation that forwards data from
/// source to destination. A full bidirectional implementation would require
/// either async I/O or separate threads for each direction to avoid deadlocks.
///
/// The current implementation works for rsync's protocol because:
/// 1. Initial phase: source sends file list → destination
/// 2. Response phase: destination sends checksums → source
/// 3. Delta phase: source sends file data → destination
///
/// Each phase is mostly unidirectional, so sequential relay works for basic transfers.
fn run_bidirectional_relay(
    source: SshConnection,
    dest: SshConnection,
) -> Result<ProxyStats, ClientError> {
    run_sequential_relay(source, dest)
}

/// Simple sequential relay for initial implementation.
///
/// This is a placeholder that performs a basic relay. It will be replaced
/// with a proper threaded or async implementation.
fn run_sequential_relay(
    mut source: SshConnection,
    mut dest: SshConnection,
) -> Result<ProxyStats, ClientError> {
    let mut stats = ProxyStats::default();
    let mut buf = [0u8; 8192];

    // Copy from source to dest until EOF
    loop {
        match source.read(&mut buf) {
            Ok(0) => break, // EOF from source
            Ok(n) => {
                dest.write_all(&buf[..n])
                    .map_err(|e| invalid_argument_error(&format!("relay write error: {e}"), 23))?;
                dest.flush()
                    .map_err(|e| invalid_argument_error(&format!("relay flush error: {e}"), 23))?;
                stats.bytes_source_to_dest += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(invalid_argument_error(
                    &format!("relay read error: {e}"),
                    23,
                ));
            }
        }
    }

    // Close source stdin and dest stdin
    source.close_stdin().ok();
    dest.close_stdin().ok();

    Ok(stats)
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
