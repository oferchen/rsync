//! SSH transfer orchestration.
//!
//! This module coordinates SSH-based remote transfers by spawning SSH connections,
//! negotiating the rsync protocol, and executing transfers using the server
//! infrastructure.
//!
//! # Architecture
//!
//! Transfers use the [`SshConnection::split`] method to obtain separate read/write
//! halves, which are then passed to the server infrastructure for protocol handling.

use std::ffi::{OsStr, OsString};

#[cfg(feature = "tracing")]
use tracing::instrument;

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, write_filter_list};
use rsync_io::ssh::{SshCommand, SshConnection, parse_ssh_operand};

use super::super::config::{ClientConfig, FilterRuleKind, FilterRuleSpec};
use super::super::error::{ClientError, invalid_argument_error};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::invocation::{
    RemoteInvocationBuilder, RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role,
};
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
#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "ssh_transfer")
)]
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

    // Determine transfer type
    let transfer_spec = determine_transfer_role(sources, destination)?;

    match transfer_spec {
        TransferSpec::Push {
            local_sources,
            remote_dest,
        } => {
            // Push: local → remote
            let (invocation_args, ssh_host, ssh_user, ssh_port) =
                parse_single_remote(&remote_dest, config, RemoteRole::Sender)?;
            let connection =
                build_ssh_connection(&ssh_user, &ssh_host, ssh_port, &invocation_args, config)?;
            run_push_transfer(config, connection, &local_sources, observer)
        }
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            // Pull: remote → local
            let (invocation_args, ssh_host, ssh_user, ssh_port) =
                parse_remote_operands(&remote_sources, config, RemoteRole::Receiver)?;
            let connection =
                build_ssh_connection(&ssh_user, &ssh_host, ssh_port, &invocation_args, config)?;
            run_pull_transfer(config, connection, &[local_dest], observer)
        }
        TransferSpec::Proxy {
            remote_sources,
            remote_dest,
        } => {
            // Proxy: remote → remote (via local)
            run_proxy_transfer(config, remote_sources, remote_dest, observer)
        }
    }
}

/// Parses a single remote operand and builds the invocation args.
#[allow(clippy::type_complexity)]
fn parse_single_remote(
    operand_str: &str,
    config: &ClientConfig,
    role: RemoteRole,
) -> Result<(Vec<OsString>, String, Option<String>, Option<u16>), ClientError> {
    let operand = parse_ssh_operand(OsStr::new(operand_str))
        .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

    let invocation_builder = RemoteInvocationBuilder::new(config, role);
    let args = invocation_builder.build(operand.path());

    Ok((
        args,
        operand.host().to_owned(),
        operand.user().map(String::from),
        operand.port(),
    ))
}

/// Parses remote operand(s) and builds the invocation args.
#[allow(clippy::type_complexity)]
fn parse_remote_operands(
    remote_operands: &RemoteOperands,
    config: &ClientConfig,
    role: RemoteRole,
) -> Result<(Vec<OsString>, String, Option<String>, Option<u16>), ClientError> {
    match remote_operands {
        RemoteOperands::Single(operand_str) => parse_single_remote(operand_str, config, role),
        RemoteOperands::Multiple(operand_strs) => {
            // Multiple sources (pull operation)
            // Parse first operand to get SSH connection details
            let first_operand = parse_ssh_operand(OsStr::new(&operand_strs[0]))
                .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

            // Parse all operands to extract paths
            let mut paths = Vec::new();
            for operand_str in operand_strs {
                let operand = parse_ssh_operand(OsStr::new(operand_str)).map_err(|e| {
                    invalid_argument_error(&format!("invalid remote operand: {e}"), 1)
                })?;
                paths.push(operand.path().to_owned());
            }

            // Build invocation with all paths
            let invocation_builder = RemoteInvocationBuilder::new(config, role);
            let path_refs: Vec<&str> = paths.iter().map(|s| s.as_ref()).collect();
            let args = invocation_builder.build_with_paths(&path_refs);

            Ok((
                args,
                first_operand.host().to_owned(),
                first_operand.user().map(String::from),
                first_operand.port(),
            ))
        }
    }
}

/// Builds and spawns an SSH connection with the remote rsync invocation.
fn build_ssh_connection(
    user: &Option<String>,
    host: &str,
    port: Option<u16>,
    invocation_args: &[OsString],
    config: &ClientConfig,
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

    // Configure custom remote shell if specified
    if let Some(shell_args) = config.remote_shell()
        && !shell_args.is_empty()
    {
        // First argument is the program name
        ssh.set_program(&shell_args[0]);
        // Remaining arguments are SSH options
        for arg in &shell_args[1..] {
            ssh.push_option(arg.clone());
        }
    }

    // Set the remote command (rsync --server ...)
    ssh.set_remote_command(invocation_args);

    // Spawn the SSH process
    // Mirror upstream: SSH spawn failures return IPC error code (pipe.c:85)
    ssh.spawn().map_err(|e| {
        invalid_argument_error(
            &format!("failed to spawn SSH connection: {e}"),
            super::super::IPC_EXIT_CODE,
        )
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
    // Send filter list to remote generator before starting the transfer
    // Use protocol v32 for filter format (backward compatible)
    let protocol = ProtocolVersion::try_from(32u8)
        .map_err(|e| invalid_argument_error(&format!("invalid protocol version: {e}"), 1))?;
    send_filter_list_to_server(&mut connection, config, protocol)?;

    // Build server config for receiver role
    // In a pull, we receive files from remote, so we're the receiver
    let server_config = build_server_config_for_receiver(config, local_paths)?;

    // Split connection into separate read/write halves and run server
    let server_stats = run_server_over_ssh_connection(server_config, connection)?;

    // Convert server stats to client summary
    Ok(convert_server_stats_to_summary(server_stats))
}

/// Executes a push transfer (local → remote).
///
/// In a push transfer, the local side acts as the sender/generator and the
/// remote side acts as the receiver. We reuse the server generator infrastructure.
fn run_push_transfer(
    config: &ClientConfig,
    connection: SshConnection,
    local_paths: &[String],
    _observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    // Build server config for generator (sender) role
    // In a push, we send files to remote, so we're the generator
    let server_config = build_server_config_for_generator(config, local_paths)?;

    // Split connection into separate read/write halves and run server
    let server_stats = run_server_over_ssh_connection(server_config, connection)?;

    // Convert server stats to client summary
    Ok(convert_server_stats_to_summary(server_stats))
}

/// Executes a proxy transfer (remote → remote via local).
///
/// In a proxy transfer, the local machine relays protocol messages between
/// two remote hosts. We spawn two SSH connections:
/// 1. To the source with `rsync --server --sender` (acts as generator)
/// 2. To the destination with `rsync --server` (acts as receiver)
///
/// Data flows: source → local (relay) → destination
fn run_proxy_transfer(
    config: &ClientConfig,
    remote_sources: RemoteOperands,
    remote_dest: String,
    _observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    use super::remote_to_remote::run_remote_to_remote_transfer;

    run_remote_to_remote_transfer(config, remote_sources, remote_dest)
}

/// Converts server-side statistics to a client summary.
///
/// Maps the statistics returned by the server (receiver or generator) into the
/// format expected by the client summary. Uses the available server statistics
/// (files listed, files transferred, and bytes sent/received) to create a
/// LocalCopySummary with the most relevant fields populated.
fn convert_server_stats_to_summary(stats: crate::server::ServerStats) -> ClientSummary {
    use crate::server::ServerStats;
    use engine::local_copy::LocalCopySummary;

    let summary = match stats {
        ServerStats::Receiver(transfer_stats) => {
            // For pull transfers: we received files from remote
            LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.bytes_received,
            )
        }
        ServerStats::Generator(generator_stats) => {
            // For push transfers: we sent files to remote
            LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.bytes_sent,
            )
        }
    };

    ClientSummary::from_summary(summary)
}

/// Runs server over an SSH connection using split read/write halves.
///
/// This uses [`SshConnection::split`] to obtain separate reader and writer handles,
/// avoiding the need for unsafe aliased mutable references.
fn run_server_over_ssh_connection(
    config: ServerConfig,
    connection: SshConnection,
) -> Result<crate::server::ServerStats, ClientError> {
    let (mut reader, mut writer, _child_handle) = connection
        .split()
        .map_err(|e| invalid_argument_error(&format!("failed to split SSH connection: {e}"), 23))?;

    crate::server::run_server_stdio(config, &mut reader, &mut writer)
        .map_err(|e| invalid_argument_error(&format!("transfer failed: {e}"), 23))
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
    #[cfg(all(unix, feature = "acl"))]
    if config.preserve_acls() {
        flags.push('A');
    }
    #[cfg(all(unix, feature = "xattr"))]
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

/// Sends filter list to remote server over SSH connection.
///
/// This is called by the local receiver before starting the transfer to send
/// filter rules to the remote generator. The remote generator will read this
/// filter list and apply it during file list generation.
fn send_filter_list_to_server(
    connection: &mut SshConnection,
    config: &ClientConfig,
    protocol: ProtocolVersion,
) -> Result<(), ClientError> {
    // Convert ClientConfig filter rules to wire format
    let wire_rules = build_wire_format_rules(config.filter_rules())?;

    // Send using protocol wire format
    write_filter_list(connection, &wire_rules, protocol)
        .map_err(|e| invalid_argument_error(&format!("failed to send filter list: {e}"), 12))?;

    #[cfg(feature = "tracing")]
    tracing::debug!(count = wire_rules.len(), "Sent filter rules to remote");

    Ok(())
}

/// Converts client filter rules to wire format.
///
/// Maps FilterRuleSpec (client-side representation) to FilterRuleWireFormat
/// (protocol wire representation) for transmission to the remote server.
fn build_wire_format_rules(
    client_rules: &[FilterRuleSpec],
) -> Result<Vec<FilterRuleWireFormat>, ClientError> {
    let mut wire_rules = Vec::new();

    for spec in client_rules {
        // Convert FilterRuleKind to RuleType
        let rule_type = match spec.kind() {
            FilterRuleKind::Include => RuleType::Include,
            FilterRuleKind::Exclude => RuleType::Exclude,
            FilterRuleKind::Clear => RuleType::Clear,
            FilterRuleKind::Protect => RuleType::Protect,
            FilterRuleKind::Risk => RuleType::Risk,
            FilterRuleKind::DirMerge => RuleType::DirMerge,
            FilterRuleKind::ExcludeIfPresent => {
                // ExcludeIfPresent is transmitted as Exclude with 'e' flag
                // (FILTRULE_EXCLUDE_SELF in upstream rsync)
                wire_rules.push(FilterRuleWireFormat {
                    rule_type: RuleType::Exclude,
                    pattern: spec.pattern().to_owned(),
                    anchored: spec.pattern().starts_with('/'),
                    directory_only: spec.pattern().ends_with('/'),
                    no_inherit: false,
                    cvs_exclude: false,
                    word_split: false,
                    exclude_from_merge: true, // 'e' flag = EXCLUDE_SELF
                    xattr_only: spec.is_xattr_only(),
                    sender_side: spec.applies_to_sender(),
                    receiver_side: spec.applies_to_receiver(),
                    perishable: spec.is_perishable(),
                    negate: false,
                });
                continue;
            }
        };

        // Build wire format rule
        let mut wire_rule = FilterRuleWireFormat {
            rule_type,
            pattern: spec.pattern().to_owned(),
            anchored: spec.pattern().starts_with('/'),
            directory_only: spec.pattern().ends_with('/'),
            no_inherit: false, // Set based on pattern modifiers if needed
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: spec.is_xattr_only(),
            sender_side: spec.applies_to_sender(),
            receiver_side: spec.applies_to_receiver(),
            perishable: spec.is_perishable(),
            negate: false,
        };

        // Handle dir_merge options if present
        if let Some(options) = spec.dir_merge_options() {
            // Extract modifiers from dir_merge options
            wire_rule.no_inherit = !options.inherit_rules();
            wire_rule.word_split = options.uses_whitespace();
            wire_rule.exclude_from_merge = options.excludes_self();
        }

        wire_rules.push(wire_rule);
    }

    Ok(wire_rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_receiver_server_config() {
        let config = ClientConfig::builder().recursive(true).times(true).build();

        let result = build_server_config_for_receiver(&config, &["dest/".to_owned()]);
        assert!(result.is_ok());

        let server_config = result.unwrap();
        assert_eq!(server_config.role, ServerRole::Receiver);
        assert_eq!(server_config.args.len(), 1);
        assert_eq!(server_config.args[0], "dest/");
    }

    #[test]
    fn builds_generator_server_config() {
        let config = ClientConfig::builder().recursive(true).times(true).build();

        let result = build_server_config_for_generator(
            &config,
            &["file1.txt".to_owned(), "file2.txt".to_owned()],
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

    #[test]
    fn converts_empty_filter_list() {
        let rules = build_wire_format_rules(&[]).expect("should convert empty list");
        assert_eq!(rules.len(), 0);
    }

    #[test]
    fn converts_simple_exclude_rule() {
        let spec = FilterRuleSpec::exclude("*.log");
        let rules = build_wire_format_rules(&[spec]).expect("should convert exclude rule");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert_eq!(rules[0].pattern, "*.log");
        assert!(!rules[0].anchored);
        assert!(!rules[0].directory_only);
    }

    #[test]
    fn converts_simple_include_rule() {
        let spec = FilterRuleSpec::include("*.txt");
        let rules = build_wire_format_rules(&[spec]).expect("should convert include rule");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Include);
        assert_eq!(rules[0].pattern, "*.txt");
        assert!(!rules[0].anchored);
        assert!(!rules[0].directory_only);
    }

    #[test]
    fn detects_anchored_pattern() {
        let spec = FilterRuleSpec::exclude("/tmp");
        let rules = build_wire_format_rules(&[spec]).expect("should convert anchored rule");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].anchored);
        assert_eq!(rules[0].pattern, "/tmp");
    }

    #[test]
    fn detects_directory_only_pattern() {
        let spec = FilterRuleSpec::exclude("cache/");
        let rules = build_wire_format_rules(&[spec]).expect("should convert directory-only rule");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].directory_only);
        assert_eq!(rules[0].pattern, "cache/");
    }

    #[test]
    fn preserves_sender_receiver_flags() {
        let spec = FilterRuleSpec::exclude("*.tmp")
            .with_sender(true)
            .with_receiver(false);
        let rules = build_wire_format_rules(&[spec]).expect("should convert side flags");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].sender_side);
        assert!(!rules[0].receiver_side);
    }

    #[test]
    fn preserves_perishable_flag() {
        let spec = FilterRuleSpec::exclude("*.swp").with_perishable(true);
        let rules = build_wire_format_rules(&[spec]).expect("should convert perishable flag");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].perishable);
    }

    #[test]
    fn preserves_xattr_only_flag() {
        let spec = FilterRuleSpec::exclude("user.*").with_xattr_only(true);
        let rules = build_wire_format_rules(&[spec]).expect("should convert xattr_only flag");

        assert_eq!(rules.len(), 1);
        assert!(rules[0].xattr_only);
    }

    #[test]
    fn converts_all_rule_types() {
        use engine::local_copy::DirMergeOptions;

        let specs = vec![
            FilterRuleSpec::include("*.txt"),
            FilterRuleSpec::exclude("*.log"),
            FilterRuleSpec::clear(),
            FilterRuleSpec::protect("important"),
            FilterRuleSpec::risk("temp"),
            FilterRuleSpec::dir_merge(".rsync-filter", DirMergeOptions::new()),
        ];

        let rules = build_wire_format_rules(&specs).expect("should convert all rule types");

        assert_eq!(rules.len(), 6);
        assert_eq!(rules[0].rule_type, RuleType::Include);
        assert_eq!(rules[1].rule_type, RuleType::Exclude);
        assert_eq!(rules[2].rule_type, RuleType::Clear);
        assert_eq!(rules[3].rule_type, RuleType::Protect);
        assert_eq!(rules[4].rule_type, RuleType::Risk);
        assert_eq!(rules[5].rule_type, RuleType::DirMerge);
    }

    #[test]
    fn transmits_exclude_if_present_rules() {
        let specs = vec![
            FilterRuleSpec::exclude("*.log"),
            FilterRuleSpec::exclude_if_present(".git"),
            FilterRuleSpec::include("*.txt"),
        ];

        let rules = build_wire_format_rules(&specs).expect("should transmit ExcludeIfPresent");

        // ExcludeIfPresent is now transmitted as Exclude with 'e' flag
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert_eq!(rules[0].pattern, "*.log");
        assert!(!rules[0].exclude_from_merge);

        // ExcludeIfPresent becomes Exclude with exclude_from_merge (EXCLUDE_SELF)
        assert_eq!(rules[1].rule_type, RuleType::Exclude);
        assert_eq!(rules[1].pattern, ".git");
        assert!(rules[1].exclude_from_merge);

        assert_eq!(rules[2].rule_type, RuleType::Include);
        assert_eq!(rules[2].pattern, "*.txt");
    }

    #[test]
    fn handles_dir_merge_options() {
        use engine::local_copy::DirMergeOptions;

        let options = DirMergeOptions::new()
            .inherit(false)
            .exclude_filter_file(true)
            .use_whitespace();

        let spec = FilterRuleSpec::dir_merge(".rsync-filter", options);
        let rules = build_wire_format_rules(&[spec]).expect("should convert dir_merge options");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::DirMerge);
        assert!(rules[0].no_inherit); // inherit(false) -> no_inherit(true)
        assert!(rules[0].exclude_from_merge); // exclude_filter_file(true)
        assert!(rules[0].word_split); // use_whitespace()
    }
}
