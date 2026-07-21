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
use std::io::BufReader;
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
use super::implied_source::implied_source_args_for_pull;
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
    server_config.connection.filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded()).map_err(
            |e| invalid_argument_error(&format!("failed to build filter rules: {e}"), 12),
        )?;
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
    server_config.connection.filter_rules =
        flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded()).map_err(
            |e| invalid_argument_error(&format!("failed to build filter rules: {e}"), 12),
        )?;
    server_config.stop_at = config.stop_at();

    // upstream: main.c:1372-1375 - pull with a local --files-from forwards the
    // file's bytes back to the remote sender via the protocol stream.
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

    // upstream: main.c:1525,1549 / io.c:427,464 / flist.c:1026 - record each
    // requested source path (or each local --files-from entry) as an implied
    // include so the receiver rejects any unrequested name (CVE-2022-29154).
    server_config.connection.implied_source_args = implied_source_args_for_pull(
        config,
        &paths,
        server_config.connection.files_from_data.as_deref(),
    );

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

/// Applies CLI `--ssh-*` overrides and `--contimeout` fallback to an `SshConfig`.
///
/// `--ssh-connect-timeout` takes precedence over `--contimeout`. When neither
/// is set, `SshConfig`'s default (30s) is preserved. When `--contimeout` is
/// explicitly disabled (value 0), the connect timeout is set to zero, mirroring
/// how the subprocess SSH path handles `TransferTimeout::Disabled`.
///
/// upstream: options.c - `--contimeout` is forwarded as SSH's `-o ConnectTimeout`.
fn apply_cli_overrides(ssh_config: &mut SshConfig, config: &ClientConfig) {
    let mut ssh_connect_timeout_set = false;

    if let Some(opts) = config.embedded_ssh_config() {
        if !opts.ciphers.is_empty() {
            ssh_config.ciphers = Some(opts.ciphers.clone());
        }

        if let Some(timeout_secs) = opts.connect_timeout_secs {
            ssh_config.connect_timeout = Duration::from_secs(timeout_secs);
            ssh_connect_timeout_set = true;
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

    // Apply --contimeout as fallback when --ssh-connect-timeout was not set.
    // This mirrors the subprocess SSH path (ssh_transfer.rs) where
    // config.connect_timeout().effective(30s) drives -o ConnectTimeout.
    if !ssh_connect_timeout_set {
        if let Some(duration) = config
            .connect_timeout()
            .effective(ssh_config.connect_timeout)
        {
            ssh_config.connect_timeout = duration;
        } else {
            // TransferTimeout::Disabled (--contimeout=0) - disable connect timeout.
            ssh_config.connect_timeout = Duration::ZERO;
        }
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

    let (reader, mut writer) = rsync_io::ssh::embedded::connect_and_exec(
        &ssh_config,
        &remote_command,
        stdin_data.as_deref(),
    )
    .map_err(|e| invalid_argument_error(&format!("embedded SSH connection failed: {e}"), 5))?;

    // upstream: io.c read_buf() uses 32KB read-ahead buffering.
    let mut reader = BufReader::with_capacity(32768, reader);

    let start = Instant::now();
    let batch_recording = batch_ctx.as_ref().map(|ctx| {
        let is_sender = server_config.role == ServerRole::Generator;
        build_batch_recording(ctx, is_sender)
    });

    let handshake =
        crate::server::perform_handshake_with_max(&mut reader, &mut writer, server_config.protocol)
            .map_err(|e| invalid_argument_error(&format!("handshake failed: {e}"), 5))?;
    let negotiated_protocol = handshake.protocol.as_u8();

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

    // Goodbye phase: drop the writer to signal EOF to the russh bridge
    // task, then bound how long we wait for the remote to ack the close.
    // A stalled remote channel here is the v0.6.1 200x-slowdown class of
    // regression - SSR-4 turns it into a typed timeout rather than a
    // multi-minute hang. The goodbye guard only fires when the underlying
    // transfer succeeded; a failed transfer carries the more informative
    // diagnostic and should not be masked by a shutdown-phase error.
    drop(writer);
    let mut channel_reader = reader.into_inner();
    let goodbye_outcome =
        channel_reader.wait_for_eof_with_timeout(rsync_io::ssh::embedded::SSH_GOODBYE_TIMEOUT);
    let elapsed = start.elapsed();

    match transfer_result {
        Ok(stats) => match goodbye_outcome {
            Ok(()) => {
                let mut summary = convert_server_stats_to_summary(stats, elapsed);
                summary.set_protocol_version(negotiated_protocol);
                Ok(summary)
            }
            Err(e) => Err(invalid_argument_error(
                &format!("embedded SSH goodbye phase failed: {e}"),
                ExitCode::Timeout.as_i32(),
            )),
        },
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
            event.flist_eof,
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

    server_config.flags.numeric_ids = crate::server::NumericIds::from_client(config.numeric_ids());
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();
    // upstream: options.c:2911-2934 - the alt-dest args (--compare-dest,
    // --copy-dest, --link-dest) live inside the `if (am_sender)` server_options
    // block, so on a pull they are never sent over the wire to the remote
    // sender; the local client IS the receiver and applies them itself in
    // try_dests_reg() (generator.c:954). Carry them onto the local receiver
    // config here so the receiver hard-links / copies / skips unchanged files
    // against the reference dirs. Without this the russh pull transferred every
    // file whole, while the daemon pull hard-linked.
    server_config.reference_directories = config.reference_directories().to_vec();
    // upstream: backup.c:make_backup() runs on the receiver, invoked from
    // generator.c/receiver.c. `make_backups` rides in the compact flag string as
    // 'b' (options.c:2630-2631), so flags.backup is already set here; but
    // --backup-dir / --suffix are long-form values finalized in the local popt
    // parse (options.c:2285-2298) and never delivered onto the receiver config.
    // On a pull the local client IS the receiver, so carry backup_dir/backup_suffix
    // here - otherwise effective_backup_suffix() falls back to "~" and the backup
    // lands beside the file instead of in --backup-dir (local/daemon pulls kept
    // them and behaved correctly).
    server_config.backup_dir = config.backup_directory().map(|p| p.display().to_string());
    server_config.backup_suffix = config
        .backup_suffix()
        .map(|s| s.to_string_lossy().into_owned());
    // upstream: --chmod is parsed into `chmod_modes` (options.c:1762) and is
    // never placed in server_options, so it is never forwarded to the remote
    // sender. On a pull the local client IS the receiver and applies the
    // modifiers itself as it reads each incoming flist entry (flist.c:905-906
    // recv_file_entry() -> tweak_mode()). Carry them onto the local receiver
    // config here; without this the russh (ssh://) pull left every regular file
    // at its source mode while local copies applied --chmod correctly.
    server_config.chmod = config.chmod().cloned();
    // upstream: build_server_flag_string no longer packs the compact 'P' letter,
    // and 'D' now tracks devices only, so carry keep_partial and specials onto
    // the local half here (mirrors --partial / --specials|--no-specials which the
    // wire generator emits long-form).
    server_config.flags.partial = config.partial();
    server_config.flags.devices = config.preserve_devices();
    server_config.flags.specials = config.preserve_specials();
    // upstream flist.c:flist_sort_and_clean prunes empty dirs on the receiver
    // (prune_empty_dirs && !am_sender); on a pull the local client IS the receiver,
    // and -m is never sent over the wire (options.c gates it on am_sender), so the
    // flag must be carried onto the local receiver config here.
    server_config.flags.prune_empty_dirs = config.prune_empty_dirs();
    // upstream generator.c:1368-1383 never creates a directory absent at the
    // destination under --existing (ignore_non_existing); on a pull the local
    // client IS the receiver and --existing is a long-form-only flag absent from
    // the compact letter string, so carry it onto the local receiver config here.
    server_config.file_selection.existing_only = config.existing_only();
    // upstream generator.c:1395 skips any file already present at the destination
    // under --ignore-existing (`if (ignore_existing > 0 && statret == 0)` early
    // goto cleanup). options.c:2911-2919 forwards --ignore-existing to the remote
    // only inside the `if (am_sender)` server_options block, so on a pull it is
    // never sent over the wire; the local client IS the receiver and applies it
    // itself. Carry it onto the local receiver config here, mirroring
    // existing_only above. Without this the ssh:// pull re-transferred and
    // overwrote existing destination files instead of skipping them.
    server_config.file_selection.ignore_existing = config.ignore_existing();
    // upstream: options.c:2194 / generator.c:1249 - a single source operand with
    // no destination implies list-only. On a pull the local client IS the
    // receiver and `list_only` is a long-form-only concern absent from the
    // compact letter string, so carry it onto the local receiver config here.
    // Mirrors the subprocess-ssh and daemon receiver builders; without it the
    // single-operand ssh:// pull renders the flist AND writes files.
    server_config.flags.list_only = config.list_only();
    // upstream: options.c:777 / receiver.c:656,1029-1050 - --delay-updates is a
    // plain receiver-side option (no am_sender gate) that stages updates into
    // the partial dir and renames them in the phase-2 sweep. options.c:2886-2892
    // forwards it to the remote only on a push; on a pull the local client IS the
    // receiver and the flag never rides the wire, so carry it here. Mirrors the
    // subprocess-ssh and daemon receiver builders; without it the ssh:// pull
    // updates files in place, defeating --delay-updates.
    server_config.write.delay_updates = config.delay_updates();
    // upstream: options.c:2912-2913 - `if (am_sender) { if (usermap) ... }`
    // forwards --usermap to the remote only on a push. On a pull the local
    // client IS the receiver and applies the uid name-map itself as it reads
    // the incoming id list (receiver/file_list/id_lists.rs). Carry it onto the
    // local receiver config here; without this the ssh:// pull silently ignored
    // --usermap while local and daemon pulls remapped ownership.
    server_config.user_mapping = config.user_mapping().cloned();
    // upstream: options.c:2915-2916 - `if (am_sender) { if (groupmap) ... }`
    // is the gid counterpart of --usermap above; same pull rationale.
    server_config.group_mapping = config.group_mapping().cloned();
    // upstream: options.c:2930-2931 - `if (am_sender && do_fsync) --fsync`.
    // --fsync is applied by the receiver, which fsync()s each committed file
    // (syscall.c do_fsync), so on a pull the local client IS the receiver and
    // must carry the flag; it rides the wire only on a push. The daemon pull
    // already sets this in apply_common_daemon_config; both ssh builders dropped
    // it, so the ssh:// pull never fsync'd its writes.
    server_config.write.fsync = config.fsync();
    // upstream: options.c:2979-2980 - `if (write_devices && am_sender)
    // --write-devices`. --write-devices makes the receiver write file content
    // in-place into an existing device node (receiver.c: write_devices &&
    // IS_DEVICE), so on a pull the local client IS the receiver and must carry
    // it; it rides the wire only on a push.
    server_config.write.write_devices = config.write_devices();
    // upstream: options.c:2641-2643 - `if (am_sender) { if (keep_dirlinks)
    // argstr[x++] = 'K'; }`. -K makes the receiver follow a symlink-to-dir at
    // the destination instead of clobbering it (receiver/directory/creation.rs),
    // so on a pull the local client IS the receiver and must carry the flag; the
    // compact 'K' letter is emitted only when the local side is the sender.
    server_config.flags.keep_dirlinks = config.keep_dirlinks();
    // upstream: options.c:2650-2655 - `if (am_sender) { if (fuzzy_basis) {
    // argstr[x++] = 'y'; ... } }`. -y/--fuzzy lets the receiver pick a similar
    // basis file for the delta, so on a pull the local client IS the receiver
    // and must carry the fuzzy level; the compact 'y' letter is emitted only
    // when the local side is the sender.
    server_config.flags.fuzzy_level = config.fuzzy_level();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
///
/// Mirrors `ssh_transfer::build_server_config_for_generator`: the sender wires
/// `--files-from` so the file list is built from the requested entry list, not
/// from a recursive walk of the source operand.
fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    server_config.flags.numeric_ids = crate::server::NumericIds::from_client(config.numeric_ids());
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();
    // upstream: build_server_flag_string no longer packs the compact 'P' letter,
    // and 'D' now tracks devices only, so carry keep_partial and specials onto
    // the local half here (mirrors --partial / --specials|--no-specials which the
    // wire generator emits long-form).
    server_config.flags.partial = config.partial();
    server_config.flags.devices = config.preserve_devices();
    server_config.flags.specials = config.preserve_specials();
    // Local-only sender optimization; never emitted onto the wire, so it is
    // carried directly onto the in-process generator's ParsedServerFlags.
    server_config.flags.parallel_delta_scan = config.parallel_delta_scan();

    // upstream: options.c:2476-2501 / main.c:1322-1328 - the local sender
    // resolves a single files-from fd: a local file (Stdin/LocalFile, or a
    // localhost:path hostspec opened locally) or the wire fd when the list is
    // hosted on the remote receiver (RemoteFile reads `--files-from=-`).
    let plan = config.files_from().resolve_for(true, config.from0());
    if let Some(path) = plan.sender_files_from_path {
        server_config.file_selection.files_from_path = Some(path);
        server_config.file_selection.from0 = plan.sender_from0;
    }

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

    /// The embedded (russh) pull receiver must carry the alt-dest reference
    /// directories onto its ServerConfig, exactly like the subprocess ssh and
    /// daemon receiver builders. On a pull the args are never forwarded to the
    /// remote sender (upstream options.c:2911-2934 gates them on am_sender), so
    /// the local receiver applies them itself in try_dests_reg()
    /// (generator.c:954). Regression guard for the `ssh://` pull that hard-linked
    /// nothing because reference_directories was empty.
    #[test]
    fn embedded_receiver_config_propagates_reference_directories() {
        use crate::client::config::ReferenceDirectoryKind;

        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .link_destination("/prev")
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(server_config.reference_directories.len(), 2);
        assert_eq!(
            server_config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(
            server_config.reference_directories[1].kind(),
            ReferenceDirectoryKind::Link
        );
        assert_eq!(
            server_config.reference_directories[1]
                .path()
                .to_str()
                .unwrap(),
            "/prev"
        );
    }

    /// The embedded (russh) pull receiver must carry --backup-dir / --suffix onto
    /// its ServerConfig. `make_backups` rides in the compact 'b' letter so
    /// flags.backup is set, but the backup directory and suffix are long-form
    /// values (upstream options.c:2285-2298) the local receiver applies itself in
    /// backup.c:make_backup(). Regression guard for the `ssh://` pull that wrote a
    /// "~" backup beside the file instead of into --backup-dir.
    #[test]
    fn embedded_receiver_config_propagates_backup_dir_and_suffix() {
        let config = ClientConfig::builder()
            .backup(true)
            .backup_directory(Some("/bak"))
            .backup_suffix(Some(".old"))
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.flags.backup);
        assert_eq!(server_config.backup_dir.as_deref(), Some("/bak"));
        assert_eq!(server_config.backup_suffix.as_deref(), Some(".old"));
    }

    /// Without --backup the receiver config carries no backup directory or suffix,
    /// so the backup path stays disabled.
    #[test]
    fn embedded_receiver_config_without_backup_has_no_backup_dir() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(!server_config.flags.backup);
        assert!(server_config.backup_dir.is_none());
        assert!(server_config.backup_suffix.is_none());
    }

    /// The embedded (russh) pull receiver must carry --ignore-existing onto its
    /// ServerConfig. Upstream generator.c:1395 skips existing dest files, and
    /// options.c:2911-2919 forwards the flag to the remote only when am_sender, so
    /// on a pull it never rides the wire. Regression guard for the `ssh://` pull
    /// that overwrote an existing destination file instead of skipping it.
    #[test]
    fn embedded_receiver_config_propagates_ignore_existing() {
        let config = ClientConfig::builder().ignore_existing(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.file_selection.ignore_existing);
    }

    /// Without --ignore-existing the receiver config leaves the flag clear.
    #[test]
    fn embedded_receiver_config_without_ignore_existing_stays_clear() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(!server_config.file_selection.ignore_existing);
    }

    /// The embedded (russh) pull receiver must carry `--chmod` onto its
    /// ServerConfig. `--chmod` is never forwarded to the remote sender (upstream
    /// options.c:1762 parses it into `chmod_modes`, absent from server_options),
    /// so on a pull the local client IS the receiver and applies it itself
    /// (flist.c:905-906). Regression guard for the `ssh://` pull that left every
    /// regular file at its source mode instead of applying `--chmod`.
    #[test]
    fn embedded_receiver_config_propagates_chmod() {
        let modifiers = ::metadata::ChmodModifiers::parse("D2755,F640").expect("parse chmod spec");
        let config = ClientConfig::builder()
            .chmod(Some(modifiers.clone()))
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(server_config.chmod.as_ref(), Some(&modifiers));
    }

    /// Without `--chmod` the embedded receiver config carries no chmod modifiers,
    /// so the destination mode is preserved exactly as sent.
    #[test]
    fn embedded_receiver_config_without_chmod_has_none() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.chmod.is_none());
    }

    /// The embedded (russh) pull receiver must carry `list_only` onto its
    /// ServerConfig for parity with the subprocess-ssh and daemon receiver
    /// builders. Upstream options.c:2194 / generator.c:1249 - a single source
    /// operand with no destination implies list-only; it is a long-form-only
    /// concern the local receiver applies itself. Regression guard for the
    /// `ssh://` single-operand pull that rendered the flist AND wrote files.
    #[test]
    fn embedded_receiver_config_propagates_list_only() {
        let config = ClientConfig::builder().list_only(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.flags.list_only);
    }

    /// The embedded (russh) pull receiver must carry `--delay-updates` onto its
    /// ServerConfig for parity with the subprocess-ssh and daemon receiver
    /// builders. Upstream options.c:777 / receiver.c:656 - a plain receiver-side
    /// option that stages updates and renames them in the phase-2 sweep; on a
    /// pull it never rides the wire. Regression guard for the `ssh://` pull that
    /// updated files in place, defeating --delay-updates.
    #[test]
    fn embedded_receiver_config_propagates_delay_updates() {
        let config = ClientConfig::builder().delay_updates(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.write.delay_updates);
    }

    /// The embedded (russh) pull receiver must carry --usermap onto its
    /// ServerConfig. Upstream options.c:2912-2913 forwards --usermap to the
    /// remote only when am_sender, so on a pull the local receiver applies the
    /// uid name-map itself (receiver/file_list/id_lists.rs).
    #[cfg(unix)]
    #[test]
    fn embedded_receiver_config_propagates_usermap() {
        let mapping = ::metadata::UserMapping::parse("*:5678").expect("parse usermap");
        let config = ClientConfig::builder()
            .user_mapping(Some(mapping.clone()))
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(server_config.user_mapping.as_ref(), Some(&mapping));
    }

    /// The gid counterpart of --usermap (upstream options.c:2915-2916).
    #[cfg(unix)]
    #[test]
    fn embedded_receiver_config_propagates_groupmap() {
        let mapping = ::metadata::GroupMapping::parse("*:1234").expect("parse groupmap");
        let config = ClientConfig::builder()
            .group_mapping(Some(mapping.clone()))
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(server_config.group_mapping.as_ref(), Some(&mapping));
    }

    /// The embedded (russh) pull receiver must carry --fsync onto its
    /// ServerConfig. Upstream options.c:2930-2931 forwards it to the remote only
    /// when am_sender, so on a pull the local receiver fsync()s its writes.
    #[test]
    fn embedded_receiver_config_propagates_fsync() {
        let config = ClientConfig::builder().fsync(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.write.fsync);
    }

    /// The embedded (russh) pull receiver must carry --write-devices onto its
    /// ServerConfig. Upstream options.c:2979-2980 forwards it to the remote only
    /// when am_sender.
    #[test]
    fn embedded_receiver_config_propagates_write_devices() {
        let config = ClientConfig::builder().write_devices(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.write.write_devices);
    }

    /// The embedded (russh) pull receiver must carry -K onto its ServerConfig.
    /// Upstream options.c:2641-2643 packs the compact 'K' only when am_sender.
    #[test]
    fn embedded_receiver_config_propagates_keep_dirlinks() {
        let config = ClientConfig::builder().keep_dirlinks(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.flags.keep_dirlinks);
    }

    /// The embedded (russh) pull receiver must carry the fuzzy level onto its
    /// ServerConfig. Upstream options.c:2650-2655 packs the compact 'y' only
    /// when am_sender.
    #[test]
    fn embedded_receiver_config_propagates_fuzzy_level() {
        let config = ClientConfig::builder().fuzzy_level(2).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(server_config.flags.fuzzy_level, 2);
    }

    /// Without any of the receiver-only options the embedded receiver config
    /// leaves them clear, so a normal `ssh://` pull is unaffected.
    #[test]
    fn embedded_receiver_config_without_receiver_only_flags_stays_clear() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(!server_config.flags.list_only);
        assert!(!server_config.write.delay_updates);
        assert!(!server_config.write.fsync);
        assert!(!server_config.write.write_devices);
        assert!(!server_config.flags.keep_dirlinks);
        assert_eq!(server_config.flags.fuzzy_level, 0);
        assert!(server_config.user_mapping.is_none());
        assert!(server_config.group_mapping.is_none());
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

    #[test]
    fn contimeout_applied_as_fallback_when_no_ssh_connect_timeout() {
        use super::super::super::TransferTimeout;
        use std::num::NonZeroU64;

        let mut ssh_config = SshConfig::default();
        let config = ClientConfig::builder()
            .connect_timeout(TransferTimeout::Seconds(NonZeroU64::new(15).unwrap()))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(ssh_config.connect_timeout, Duration::from_secs(15));
    }

    #[test]
    fn ssh_connect_timeout_takes_precedence_over_contimeout() {
        use super::super::super::TransferTimeout;
        use std::num::NonZeroU64;

        let mut ssh_config = SshConfig::default();
        let config = ClientConfig::builder()
            .connect_timeout(TransferTimeout::Seconds(NonZeroU64::new(60).unwrap()))
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                connect_timeout_secs: Some(10),
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(
            ssh_config.connect_timeout,
            Duration::from_secs(10),
            "--ssh-connect-timeout should take precedence over --contimeout"
        );
    }

    #[test]
    fn contimeout_disabled_sets_zero_timeout() {
        use super::super::super::TransferTimeout;

        let mut ssh_config = SshConfig::default();
        let original_timeout = ssh_config.connect_timeout;
        assert_ne!(
            original_timeout,
            Duration::ZERO,
            "precondition: default is non-zero"
        );

        let config = ClientConfig::builder()
            .connect_timeout(TransferTimeout::Disabled)
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(
            ssh_config.connect_timeout,
            Duration::ZERO,
            "--contimeout=0 should disable the connect timeout"
        );
    }

    #[test]
    fn contimeout_default_preserves_ssh_config_default() {
        use super::super::super::TransferTimeout;

        let mut ssh_config = SshConfig::default();
        let original_timeout = ssh_config.connect_timeout;

        let config = ClientConfig::builder()
            .connect_timeout(TransferTimeout::Default)
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert_eq!(
            ssh_config.connect_timeout, original_timeout,
            "default --contimeout should preserve SshConfig's default timeout"
        );
    }

    #[test]
    fn contimeout_with_embedded_ssh_config_but_no_ssh_timeout() {
        use super::super::super::TransferTimeout;
        use std::num::NonZeroU64;

        let mut ssh_config = SshConfig::default();
        let config = ClientConfig::builder()
            .connect_timeout(TransferTimeout::Seconds(NonZeroU64::new(25).unwrap()))
            .embedded_ssh_config(Some(EmbeddedSshOptions {
                no_agent: true,
                ..Default::default()
            }))
            .build();

        apply_cli_overrides(&mut ssh_config, &config);
        assert!(!ssh_config.use_agent, "--ssh-no-agent should still apply");
        assert_eq!(
            ssh_config.connect_timeout,
            Duration::from_secs(25),
            "--contimeout should apply when --ssh-connect-timeout is not set"
        );
    }
}
