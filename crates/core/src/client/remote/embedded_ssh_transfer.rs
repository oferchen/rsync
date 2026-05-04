//! Embedded SSH transfer orchestration using the russh library.
//!
//! Provides a pure-Rust alternative to spawning the system `ssh` binary for
//! `ssh://` URL transfers. Feature-gated behind `embedded-ssh`. The module
//! reuses the same server infrastructure as the system SSH path - only the
//! connection establishment differs.
//!
//! # Architecture
//!
//! 1. Parse the `ssh://` URL via `SshConfig::from_url()`
//! 2. Apply CLI overrides from `EmbeddedSshOptions`
//! 3. Resolve the host, connect, and authenticate using russh
//! 4. Open a channel and exec the remote `rsync --server` command
//! 5. Wrap the channel's async I/O in synchronous `Read`/`Write` adapters
//! 6. Hand off to `crate::server` for protocol negotiation and transfer
//!
//! # Upstream Reference
//!
//! - `main.c:do_cmd()` - SSH fork/exec and pipe setup (replaced by russh)
//! - `main.c:client_run()` - Role dispatch after SSH connection

use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "tracing")]
use tracing::instrument;

use engine::batch::BatchWriter;
use rsync_io::ssh::embedded::SshConfig;

use super::super::config::ClientConfig;
#[allow(unused_imports)] // REASON: used in tests
use super::super::config::EmbeddedSshOptions;
use super::super::error::{ClientError, invalid_argument_error};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::batch_support::{build_batch_context, build_batch_recording};
use super::flags;
use super::invocation::{
    RemoteInvocationBuilder, RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role,
};
use super::ssh_transfer::convert_server_stats_to_summary;
use crate::exit_code::ExitCode;
use crate::server::{ServerConfig, ServerRole, TransferProgressCallback, TransferProgressEvent};

/// Checks whether an operand is an `ssh://` URL suitable for embedded transport.
///
/// Returns `true` for operands starting with `ssh://`, which distinguishes
/// embedded SSH transfers from standard `host:path` SSH operands that use
/// the system SSH binary.
pub(crate) fn is_ssh_url(operand: &str) -> bool {
    operand.starts_with("ssh://")
}

/// Executes a transfer over the embedded SSH transport.
///
/// Entry point for `ssh://` URL transfers when the `embedded-ssh` feature is
/// enabled. Mirrors `run_ssh_transfer` but replaces the system SSH binary
/// with the russh-based pure-Rust transport.
///
/// # Arguments
///
/// * `config` - Client configuration with transfer options
/// * `observer` - Optional progress observer
/// * `batch_writer` - Optional batch recording writer
///
/// # Errors
///
/// Returns error if:
/// - URL parsing fails
/// - DNS resolution fails
/// - SSH connection or authentication fails
/// - Remote command execution fails
/// - Protocol negotiation fails
/// - Transfer execution fails
#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "embedded_ssh_transfer")
)]
pub fn run_embedded_ssh_transfer(
    config: &ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
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
        } => run_embedded_push(config, &remote_dest, &local_sources, observer, batch_writer),
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => run_embedded_pull(config, &remote_sources, &local_dest, observer, batch_writer),
        TransferSpec::Proxy { .. } => Err(invalid_argument_error(
            "remote-to-remote proxy transfers are not supported with embedded SSH",
            1,
        )),
    }
}

/// Executes a push transfer (local -> remote) via embedded SSH.
fn run_embedded_push(
    config: &ClientConfig,
    remote_dest: &str,
    local_sources: &[String],
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    let (ssh_config, remote_path) = parse_ssh_url(remote_dest, config)?;
    let invocation_builder = RemoteInvocationBuilder::new(config, RemoteRole::Sender);
    let secluded = invocation_builder.build_secluded(&[&remote_path]);

    let mut server_config = build_server_config_for_generator(config, local_sources)?;
    server_config.connection.client_mode = true;
    server_config.connection.filter_rules = flags::build_wire_format_rules(config.filter_rules())
        .map_err(|e| {
        invalid_argument_error(&format!("failed to build filter rules: {e}"), 12)
    })?;
    server_config.stop_at = config.stop_at();

    let batch_ctx = batch_writer.map(|bw| build_batch_context(config, bw));

    run_transfer_over_embedded_ssh(
        ssh_config,
        &secluded.command_line_args,
        &secluded.stdin_args,
        server_config,
        observer,
        batch_ctx,
    )
}

/// Executes a pull transfer (remote -> local) via embedded SSH.
fn run_embedded_pull(
    config: &ClientConfig,
    remote_sources: &RemoteOperands,
    local_dest: &str,
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    let (ssh_config, paths) = parse_remote_operands_urls(remote_sources, config)?;
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let invocation_builder = RemoteInvocationBuilder::new(config, RemoteRole::Receiver);
    let secluded = invocation_builder.build_secluded(&path_refs);

    let mut server_config = build_server_config_for_receiver(config, &[local_dest.to_owned()])?;
    server_config.connection.client_mode = true;
    server_config.connection.filter_rules = flags::build_wire_format_rules(config.filter_rules())
        .map_err(|e| {
        invalid_argument_error(&format!("failed to build filter rules: {e}"), 12)
    })?;
    server_config.stop_at = config.stop_at();

    let batch_ctx = batch_writer.map(|bw| build_batch_context(config, bw));

    run_transfer_over_embedded_ssh(
        ssh_config,
        &secluded.command_line_args,
        &secluded.stdin_args,
        server_config,
        observer,
        batch_ctx,
    )
}

/// Parses an `ssh://` URL and applies CLI overrides from `EmbeddedSshOptions`.
fn parse_ssh_url(url: &str, config: &ClientConfig) -> Result<(SshConfig, String), ClientError> {
    let (mut ssh_config, remote_path) = SshConfig::from_url(url)
        .map_err(|e| invalid_argument_error(&format!("invalid ssh:// URL: {e}"), 1))?;

    apply_cli_overrides(&mut ssh_config, config);

    Ok((ssh_config, remote_path))
}

/// Parses remote operands (all `ssh://` URLs) and returns a single `SshConfig`.
fn parse_remote_operands_urls(
    operands: &RemoteOperands,
    config: &ClientConfig,
) -> Result<(SshConfig, Vec<String>), ClientError> {
    match operands {
        RemoteOperands::Single(url) => {
            let (ssh_config, path) = parse_ssh_url(url, config)?;
            Ok((ssh_config, vec![path]))
        }
        RemoteOperands::Multiple(urls) => {
            let mut ssh_config = None;
            let mut paths = Vec::with_capacity(urls.len());

            for url in urls {
                let (cfg, path) = parse_ssh_url(url, config)?;
                if let Some(ref existing) = ssh_config {
                    let existing: &SshConfig = existing;
                    if cfg.host != existing.host
                        || cfg.port != existing.port
                        || cfg.username != existing.username
                    {
                        return Err(invalid_argument_error(
                            "all ssh:// sources must use the same host, port, and user",
                            1,
                        ));
                    }
                } else {
                    ssh_config = Some(cfg);
                }
                paths.push(path);
            }

            Ok((ssh_config.expect("at least one URL in Multiple"), paths))
        }
    }
}

/// Applies CLI `--ssh-*` overrides to an `SshConfig`.
fn apply_cli_overrides(ssh_config: &mut SshConfig, config: &ClientConfig) {
    let Some(opts) = config.embedded_ssh_config() else {
        return;
    };

    if !opts.ciphers.is_empty() {
        ssh_config.ciphers = Some(opts.ciphers.clone());
    }

    if let Some(timeout_secs) = opts.connect_timeout_secs {
        ssh_config.connect_timeout = Duration::from_secs(timeout_secs);
    }

    if let Some(keepalive_secs) = opts.keepalive_interval_secs {
        ssh_config.keepalive_interval = if keepalive_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(keepalive_secs))
        };
    }

    if !opts.identity_files.is_empty() {
        ssh_config.identity_files = opts.identity_files.clone();
    }

    if opts.no_agent {
        ssh_config.use_agent = false;
    }

    if let Some(ref policy) = opts.strict_host_key_checking {
        use rsync_io::ssh::embedded::StrictHostKeyChecking;
        ssh_config.strict_host_key_checking = match policy.as_str() {
            "yes" => StrictHostKeyChecking::Yes,
            "no" => StrictHostKeyChecking::No,
            _ => StrictHostKeyChecking::Ask,
        };
    }

    if opts.prefer_ipv6 {
        use rsync_io::ssh::embedded::IpPreference;
        ssh_config.ip_preference = IpPreference::PreferV6;
    }

    if let Some(port) = opts.port {
        ssh_config.port = port;
    }
}

/// Connects, authenticates, and runs a transfer over the embedded SSH transport.
///
/// Delegates to `rsync_io::ssh::embedded::connect_and_exec` for the connection
/// lifecycle, then hands the sync I/O handles to the server infrastructure for
/// protocol negotiation and transfer execution.
fn run_transfer_over_embedded_ssh(
    ssh_config: SshConfig,
    invocation_args: &[OsString],
    stdin_args: &[String],
    server_config: ServerConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_ctx: Option<super::batch_support::BatchContext>,
) -> Result<ClientSummary, ClientError> {
    let remote_command = build_remote_command(invocation_args);

    let stdin_data = if stdin_args.is_empty() {
        None
    } else {
        let mut data = Vec::new();
        for arg in stdin_args {
            data.extend_from_slice(arg.as_bytes());
            data.push(0);
        }
        data.push(0);
        Some(data)
    };

    let (mut reader, mut writer) = rsync_io::ssh::embedded::connect_and_exec(
        &ssh_config,
        &remote_command,
        stdin_data.as_deref(),
    )
    .map_err(|e| invalid_argument_error(&format!("embedded SSH connection failed: {e}"), 5))?;

    let start = Instant::now();
    let batch_recording = batch_ctx.as_ref().map(|ctx| {
        let is_sender = server_config.role == ServerRole::Generator;
        build_batch_recording(ctx, is_sender)
    });

    let handshake = crate::server::perform_handshake(&mut reader, &mut writer)
        .map_err(|e| invalid_argument_error(&format!("handshake failed: {e}"), 5))?;

    let mut adapter = observer.map(|obs| ServerProgressAdapter::new(obs, start));
    let progress: Option<&mut dyn TransferProgressCallback> = adapter
        .as_mut()
        .map(|a| a as &mut dyn TransferProgressCallback);

    let transfer_result = crate::server::run_server_with_handshake(
        server_config,
        handshake,
        &mut reader,
        &mut writer,
        progress,
        batch_recording,
        None,
    );

    drop(writer);
    let elapsed = start.elapsed();

    match transfer_result {
        Ok(stats) => Ok(convert_server_stats_to_summary(stats, elapsed)),
        Err(e) => {
            let exit = ExitCode::from_io_error(&e);
            Err(invalid_argument_error(
                &format!("transfer failed: {e}"),
                exit.as_i32(),
            ))
        }
    }
}

/// Builds the remote command string from invocation arguments.
fn build_remote_command(args: &[OsString]) -> String {
    args.iter()
        .map(|a| shell_escape(a.to_string_lossy().as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Shell-escapes a string for safe inclusion in a remote command.
fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./=".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Adapts a [`ClientProgressObserver`] to [`TransferProgressCallback`].
///
/// Identical to the adapter in `ssh_transfer.rs` - duplicated to keep
/// module independence while both converge on the same server infrastructure.
struct ServerProgressAdapter<'a> {
    observer: &'a mut dyn ClientProgressObserver,
    start: Instant,
    overall_transferred: u64,
}

impl<'a> ServerProgressAdapter<'a> {
    fn new(observer: &'a mut dyn ClientProgressObserver, start: Instant) -> Self {
        Self {
            observer,
            start,
            overall_transferred: 0,
        }
    }
}

impl TransferProgressCallback for ServerProgressAdapter<'_> {
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>) {
        use std::path::Path;
        use std::sync::Arc;

        self.overall_transferred += event.file_bytes;

        let client_event = super::super::summary::ClientEvent::from_progress(
            event.path,
            event.file_bytes,
            event.total_file_bytes,
            self.start.elapsed(),
            Arc::from(Path::new("")),
        );

        let update = super::super::progress::ClientProgressUpdate::from_transfer_event(
            client_event,
            event.files_done,
            event.total_files,
            event.total_file_bytes,
            self.overall_transferred,
            self.start.elapsed(),
        );

        self.observer.on_progress(&update);
    }
}

/// Builds server configuration for receiver role (pull transfer).
fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    server_config.flags.numeric_ids = config.numeric_ids();
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    server_config.flags.numeric_ids = config.numeric_ids();
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_ssh_url_detects_ssh_scheme() {
        assert!(is_ssh_url("ssh://host/path"));
        assert!(is_ssh_url("ssh://user@host/path"));
        assert!(is_ssh_url("ssh://user:pass@host:2222/path"));
    }

    #[test]
    fn is_ssh_url_rejects_non_ssh() {
        assert!(!is_ssh_url("rsync://host/module"));
        assert!(!is_ssh_url("host:path"));
        assert!(!is_ssh_url("host::module"));
        assert!(!is_ssh_url("/local/path"));
    }

    #[test]
    fn shell_escape_simple_string() {
        assert_eq!(shell_escape("rsync"), "rsync");
        assert_eq!(shell_escape("--server"), "--server");
        assert_eq!(shell_escape("."), ".");
    }

    #[test]
    fn shell_escape_string_with_spaces() {
        assert_eq!(shell_escape("path with spaces"), "'path with spaces'");
    }

    #[test]
    fn shell_escape_string_with_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn build_remote_command_joins_args() {
        let args = vec![
            OsString::from("rsync"),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("."),
            OsString::from("/path/to/data"),
        ];
        let cmd = build_remote_command(&args);
        assert_eq!(cmd, "rsync --server --sender . /path/to/data");
    }

    #[test]
    fn build_remote_command_escapes_special_chars() {
        let args = vec![OsString::from("rsync"), OsString::from("path with space")];
        let cmd = build_remote_command(&args);
        assert!(cmd.contains("'path with space'"));
    }

    #[test]
    fn apply_cli_overrides_ciphers() {
        let mut ssh_config = SshConfig::default();
        ssh_config.host("example.com");

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                ciphers: vec!["aes256-ctr".to_owned()],
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(ssh_config.ciphers, Some(vec!["aes256-ctr".to_owned()]));
    }

    #[test]
    fn apply_cli_overrides_timeout() {
        let mut ssh_config = SshConfig::default();

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                connect_timeout_secs: Some(10),
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(ssh_config.connect_timeout, Duration::from_secs(10));
    }

    #[test]
    fn apply_cli_overrides_disable_keepalive() {
        let mut ssh_config = SshConfig::default();
        assert!(ssh_config.keepalive_interval.is_some());

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                keepalive_interval_secs: Some(0),
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert!(ssh_config.keepalive_interval.is_none());
    }

    #[test]
    fn apply_cli_overrides_no_agent() {
        let mut ssh_config = SshConfig::default();
        assert!(ssh_config.use_agent);

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                no_agent: true,
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert!(!ssh_config.use_agent);
    }

    #[test]
    fn apply_cli_overrides_strict_host_key_yes() {
        let mut ssh_config = SshConfig::default();

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                strict_host_key_checking: Some("yes".to_owned()),
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(
            ssh_config.strict_host_key_checking,
            rsync_io::ssh::embedded::StrictHostKeyChecking::Yes,
        );
    }

    #[test]
    fn apply_cli_overrides_port() {
        let mut ssh_config = SshConfig::default();

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                port: Some(2222),
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(ssh_config.port, 2222);
    }

    #[test]
    fn apply_cli_overrides_identity_files() {
        let mut ssh_config = SshConfig::default();
        let original_count = ssh_config.identity_files.len();
        assert!(original_count > 0);

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                identity_files: vec![std::path::PathBuf::from("/custom/key")],
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(ssh_config.identity_files.len(), 1);
        assert_eq!(
            ssh_config.identity_files[0],
            std::path::PathBuf::from("/custom/key")
        );
    }

    #[test]
    fn apply_cli_overrides_ipv6() {
        let mut ssh_config = SshConfig::default();

        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                prefer_ipv6: true,
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(
            ssh_config.ip_preference,
            rsync_io::ssh::embedded::IpPreference::PreferV6,
        );
    }

    #[test]
    fn apply_cli_overrides_none_is_noop() {
        let mut ssh_config = SshConfig::default();
        let original_port = ssh_config.port;
        let original_timeout = ssh_config.connect_timeout;

        let config = ClientConfig::builder().build();
        apply_cli_overrides(&mut ssh_config, &config);

        assert_eq!(ssh_config.port, original_port);
        assert_eq!(ssh_config.connect_timeout, original_timeout);
    }

    #[test]
    fn parse_ssh_url_basic() {
        let config = ClientConfig::builder().build();
        let (ssh_config, path) = parse_ssh_url("ssh://user@host/~/data", &config).unwrap();
        assert_eq!(ssh_config.host, "host");
        assert_eq!(ssh_config.username.as_deref(), Some("user"));
        assert_eq!(path, "~/data");
    }

    #[test]
    fn parse_ssh_url_invalid_scheme() {
        let config = ClientConfig::builder().build();
        let result = parse_ssh_url("http://host/path", &config);
        assert!(result.is_err());
    }

    #[test]
    fn parse_ssh_url_with_port() {
        let config = ClientConfig::builder().build();
        let (ssh_config, _) = parse_ssh_url("ssh://host:2222/path", &config).unwrap();
        assert_eq!(ssh_config.port, 2222);
    }

    #[test]
    fn parse_ssh_url_cli_port_overrides_url() {
        let config = ClientConfig::builder()
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                port: Some(3333),
                ..Default::default()
            }))
            .build();
        let (ssh_config, _) = parse_ssh_url("ssh://host:2222/path", &config).unwrap();
        assert_eq!(ssh_config.port, 3333);
    }

    #[test]
    fn parse_remote_operands_single() {
        let config = ClientConfig::builder().build();
        let operands = RemoteOperands::Single("ssh://user@host/~/data".to_owned());
        let (ssh_config, paths) = parse_remote_operands_urls(&operands, &config).unwrap();
        assert_eq!(ssh_config.host, "host");
        assert_eq!(paths, vec!["~/data"]);
    }

    #[test]
    fn parse_remote_operands_multiple_same_host() {
        let config = ClientConfig::builder().build();
        let operands = RemoteOperands::Multiple(vec![
            "ssh://user@host/~/file1".to_owned(),
            "ssh://user@host/~/file2".to_owned(),
        ]);
        let (ssh_config, paths) = parse_remote_operands_urls(&operands, &config).unwrap();
        assert_eq!(ssh_config.host, "host");
        assert_eq!(paths, vec!["~/file1", "~/file2"]);
    }

    #[test]
    fn parse_remote_operands_multiple_different_hosts_fails() {
        let config = ClientConfig::builder().build();
        let operands = RemoteOperands::Multiple(vec![
            "ssh://user@host1/~/file1".to_owned(),
            "ssh://user@host2/~/file2".to_owned(),
        ]);
        let result = parse_remote_operands_urls(&operands, &config);
        assert!(result.is_err());
    }

    #[test]
    fn run_embedded_ssh_transfer_rejects_insufficient_args() {
        let config = ClientConfig::builder()
            .transfer_args(vec!["ssh://host/path"])
            .build();
        let result = run_embedded_ssh_transfer(&config, None, None);
        assert!(result.is_err());
    }
}
