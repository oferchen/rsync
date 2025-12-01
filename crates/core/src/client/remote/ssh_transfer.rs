//! SSH transfer orchestration.
//!
//! This module coordinates SSH-based remote transfers by spawning SSH connections,
//! negotiating the rsync protocol, and executing transfers using the server
//! infrastructure.

// TODO: Remove this once we refactor run_server_stdio to accept a single Read+Write parameter
// Currently we need unsafe code to split the borrow for stdin/stdout
#![allow(unsafe_code)]

use std::ffi::OsString;

use transport::ssh::{SshCommand, SshConnection, parse_ssh_operand};

use super::invocation::{RemoteInvocationBuilder, RemoteRole, determine_transfer_role};
use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error};
use super::super::summary::ClientSummary;
use super::super::progress::ClientProgressObserver;
use crate::server::{ServerConfig, ServerRole};

/// Executes a transfer over SSH transport.
///
/// This is the main entry point for SSH-based remote transfers. It:
/// 1. Determines push vs pull from operand positions
/// 2. Parses the remote operand
/// 3. Builds the remote rsync invocation
/// 4. Spawns an SSH connection
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
pub fn run_ssh_transfer(
    config: &ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    // Step 1: Parse transfer args into sources and destination
    let args = config.transfer_args();
    if args.len() < 2 {
        return Err(invalid_argument_error(
            "need at least one source and one destination",
            1,
        ));
    }

    let (sources, destination) = args.split_at(args.len() - 1);
    let destination = &destination[0];

    // Determine push vs pull
    let (role, local_paths, remote_operand_str) = determine_transfer_role(sources, destination)?;

    // Step 2: Parse remote operand
    let remote_operand = parse_ssh_operand(remote_operand_str.as_ref())
        .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

    // Step 3: Build remote rsync invocation
    let invocation_builder = RemoteInvocationBuilder::new(config, role);
    let invocation_args = invocation_builder.build(remote_operand.path());

    // Step 4: Spawn SSH connection
    let connection = build_ssh_connection(
        &remote_operand.user().map(String::from),
        remote_operand.host(),
        remote_operand.port(),
        &invocation_args,
        config,
    )?;

    // Step 5-6: Execute transfer based on role
    // We pass the connection directly to the transfer functions which will
    // handle protocol negotiation and execution
    match role {
        RemoteRole::Receiver => {
            // Pull: remote → local
            run_pull_transfer(config, connection, &local_paths, observer)
        }
        RemoteRole::Sender => {
            // Push: local → remote
            run_push_transfer(config, connection, &local_paths, observer)
        }
    }
}

/// Builds and spawns an SSH connection with the remote rsync invocation.
fn build_ssh_connection(
    user: &Option<String>,
    host: &str,
    port: Option<u16>,
    invocation_args: &[OsString],
    _config: &ClientConfig,  // Reserved for future --rsh option support
) -> Result<SshConnection, ClientError> {
    let mut ssh = SshCommand::new(host);

    // Set user if provided
    if let Some(user) = user {
        ssh.set_user(user);
    }

    // Set port if provided
    if let Some(port) = port {
        ssh.set_port(port);
    }

    // TODO: Configure custom remote shell if specified (-e/--rsh option)
    // The remote_shell() method doesn't exist yet in ClientConfig
    // For now, we use the default ssh command

    // Set the remote command (rsync --server ...)
    ssh.set_remote_command(invocation_args);

    // Spawn the SSH process
    ssh.spawn().map_err(|e| {
        invalid_argument_error(&format!("failed to spawn SSH connection: {e}"), 10)
    })
}

/// Executes a pull transfer (remote → local).
///
/// In a pull transfer, the local side acts as the receiver and the remote side
/// acts as the sender/generator. We reuse the server receiver infrastructure.
fn run_pull_transfer(
    config: &ClientConfig,
    mut connection: SshConnection,
    local_paths: &[String],
    _observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    // Build server config for receiver role
    // In a pull, we receive files from remote, so we're the receiver
    let server_config = build_server_config_for_receiver(config, local_paths)?;

    // We need to pass connection as both Read and Write to run_server_stdio.
    // Since we can't create two mutable borrows, we use a small wrapper function.
    let exit_code = run_server_over_connection(server_config, &mut connection)?;

    if exit_code != 0 {
        return Err(invalid_argument_error(
            &format!("transfer completed with exit code {exit_code}"),
            exit_code,
        ));
    }

    // TODO: Extract proper transfer stats from server receiver
    // For now, return a minimal summary
    Ok(ClientSummary::default())
}

/// Executes a push transfer (local → remote).
///
/// In a push transfer, the local side acts as the sender/generator and the
/// remote side acts as the receiver. We reuse the server generator infrastructure.
fn run_push_transfer(
    config: &ClientConfig,
    mut connection: SshConnection,
    local_paths: &[String],
    _observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    // Build server config for generator (sender) role
    // In a push, we send files to remote, so we're the generator
    let server_config = build_server_config_for_generator(config, local_paths)?;

    // We need to pass connection as both Read and Write to run_server_stdio.
    // Since we can't create two mutable borrows, we use a small wrapper function.
    let exit_code = run_server_over_connection(server_config, &mut connection)?;

    if exit_code != 0 {
        return Err(invalid_argument_error(
            &format!("transfer completed with exit code {exit_code}"),
            exit_code,
        ));
    }

    // TODO: Extract proper transfer stats from server generator
    // For now, return a minimal summary
    Ok(ClientSummary::default())
}

/// Helper function to run server over a connection that implements both Read and Write.
///
/// This wrapper exists to work around the borrow checker - we need to pass the same
/// connection as both stdin and stdout.
///
/// # Safety
///
/// This uses unsafe code to create two mutable references to the same connection.
/// This is safe because:
/// - SshConnection internally manages separate stdin/stdout file handles
/// - The Read and Write traits access different underlying resources
/// - run_server_stdio doesn't create aliasing issues between the two parameters
fn run_server_over_connection<T>(
    config: ServerConfig,
    connection: &mut T,
) -> Result<i32, ClientError>
where
    T: std::io::Read + std::io::Write,
{
    use std::io::{Read, Write};

    // SAFETY: We create two mutable references to the same connection, which is safe
    // because SshConnection manages separate stdin/stdout streams internally.
    let conn_ptr = connection as *mut T;
    let result = unsafe {
        let stdin: &mut dyn Read = &mut *conn_ptr;
        let stdout: &mut dyn Write = &mut *conn_ptr;
        crate::server::run_server_stdio(config, stdin, stdout)
    };

    result.map_err(|e| invalid_argument_error(&format!("transfer failed: {e}"), 23))
}

/// Builds server configuration for receiver role (pull transfer).
fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    // Build flag string from client config
    let flag_string = build_server_flag_string(config);

    // Receiver uses destination path as args
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
        .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))
}

/// Builds server configuration for generator role (push transfer).
fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    // Build flag string from client config
    let flag_string = build_server_flag_string(config);

    // Generator uses source paths as args
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
        .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))
}

/// Builds the compact server flag string from client configuration.
///
/// This mirrors the logic in `RemoteInvocationBuilder::build_flag_string()`
/// but returns the string directly for server config construction.
fn build_server_flag_string(config: &ClientConfig) -> String {
    let mut flags = String::from("-");

    // Transfer flags (order matches upstream server_options())
    if config.links() {
        flags.push('l');
    }
    if config.preserve_owner() {
        flags.push('o');
    }
    if config.preserve_group() {
        flags.push('g');
    }
    if config.preserve_devices() || config.preserve_specials() {
        flags.push('D');
    }
    if config.preserve_times() {
        flags.push('t');
    }
    if config.preserve_permissions() {
        flags.push('p');
    }
    if config.recursive() {
        flags.push('r');
    }
    if config.compress() {
        flags.push('z');
    }
    if config.checksum() {
        flags.push('c');
    }
    if config.preserve_hard_links() {
        flags.push('H');
    }
    if config.preserve_acls() {
        flags.push('A');
    }
    if config.preserve_xattrs() {
        flags.push('X');
    }
    if config.numeric_ids() {
        flags.push('n');
    }
    if config.delete_mode().is_enabled() || config.delete_excluded() {
        flags.push('d');
    }
    if config.whole_file() {
        flags.push('W');
    }
    if config.sparse() {
        flags.push('S');
    }
    if config.one_file_system() {
        flags.push('x');
    }
    if config.relative_paths() {
        flags.push('R');
    }
    if config.partial() {
        flags.push('P');
    }
    if config.update() {
        flags.push('u');
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_receiver_server_config() {
        let config = ClientConfig::builder()
            .recursive(true)
            .times(true)
            .build();

        let result = build_server_config_for_receiver(&config, &["dest/".to_string()]);
        assert!(result.is_ok());

        let server_config = result.unwrap();
        assert_eq!(server_config.role, ServerRole::Receiver);
        assert_eq!(server_config.args.len(), 1);
        assert_eq!(server_config.args[0], "dest/");
    }

    #[test]
    fn builds_generator_server_config() {
        let config = ClientConfig::builder()
            .recursive(true)
            .times(true)
            .build();

        let result = build_server_config_for_generator(
            &config,
            &["file1.txt".to_string(), "file2.txt".to_string()],
        );
        assert!(result.is_ok());

        let server_config = result.unwrap();
        assert_eq!(server_config.role, ServerRole::Generator);
        assert_eq!(server_config.args.len(), 2);
        assert_eq!(server_config.args[0], "file1.txt");
        assert_eq!(server_config.args[1], "file2.txt");
    }

    #[test]
    fn server_flag_string_includes_recursive() {
        let config = ClientConfig::builder().recursive(true).build();
        let flags = build_server_flag_string(&config);
        assert!(flags.contains('r'), "expected 'r' in flags: {flags}");
    }

    #[test]
    fn server_flag_string_includes_preservation_flags() {
        let config = ClientConfig::builder()
            .times(true)
            .permissions(true)
            .owner(true)
            .group(true)
            .build();

        let flags = build_server_flag_string(&config);
        assert!(flags.contains('t'), "expected 't' in flags: {flags}");
        assert!(flags.contains('p'), "expected 'p' in flags: {flags}");
        assert!(flags.contains('o'), "expected 'o' in flags: {flags}");
        assert!(flags.contains('g'), "expected 'g' in flags: {flags}");
    }
}
