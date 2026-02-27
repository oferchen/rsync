//! Remote rsync invocation builder.
//!
//! This module constructs the command-line arguments for invoking rsync in
//! `--server` mode on a remote host via SSH. The invocation format mirrors
//! upstream rsync's `server_options()` function.

use std::ffi::{OsStr, OsString};

use super::super::config::{
    ClientConfig, DeleteMode, ReferenceDirectoryKind, StrongChecksumAlgorithm, TransferTimeout,
};
use super::super::error::{ClientError, invalid_argument_error};
use compress::algorithm::CompressionAlgorithm;

/// Checks if an operand represents a remote path.
///
/// This is a simplified version that matches the logic in
/// `engine::local_copy::operand_is_remote` which is not public.
pub fn operand_is_remote(path: &OsStr) -> bool {
    let text = path.to_string_lossy();

    if text.starts_with("rsync://") {
        return true;
    }

    if text.contains("::") {
        return true;
    }

    if let Some(colon_index) = text.find(':') {
        #[cfg(windows)]
        if colon_index == 1
            && text
                .chars()
                .next()
                .map_or(false, |c| c.is_ascii_alphabetic())
        {
            return false; // Windows drive letter
        }

        let before = &text[..colon_index];
        if before.contains('/') || before.contains('\\') {
            return false;
        }

        return true;
    }

    false
}

/// Role of the local rsync process in an SSH transfer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RemoteRole {
    /// Local process is the sender (remote is receiver).
    ///
    /// Used for push operations: `oc-rsync local.txt user@host:remote.txt`
    Sender,

    /// Local process is the receiver (remote is sender).
    ///
    /// Used for pull operations: `oc-rsync user@host:remote.txt local.txt`
    Receiver,

    /// Local process is a proxy relaying between two remote hosts.
    ///
    /// Used for remote-to-remote transfers: `oc-rsync user@src:file user@dst:file`
    /// The local machine spawns two SSH connections and relays protocol messages.
    Proxy,
}

/// Parsed components of a remote operand for validation.
///
/// Used internally to ensure multiple remote sources are from the same host.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteOperandParsed {
    /// Full operand string (e.g., "user@host:/path").
    operand: String,
    /// Host portion (e.g., "host" or "192.168.1.1" or "[::1]").
    host: String,
    /// Optional user portion (e.g., "user").
    user: Option<String>,
    /// Optional port (extracted from host if present).
    port: Option<u16>,
}

/// Represents one or more remote operands in a transfer.
///
/// For push operations (local → remote), there's always a single remote destination.
/// For pull operations (remote → local), there can be multiple remote sources from
/// the same host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteOperands {
    /// Single remote operand (for push or single-source pull).
    Single(String),

    /// Multiple remote operands (for multi-source pull).
    ///
    /// All operands must be from the same host with the same user and port.
    Multiple(Vec<String>),
}

/// Full specification of a transfer, capturing both endpoints and their types.
///
/// This enum replaces the previous tuple return type of `determine_transfer_role`
/// to provide a cleaner, more explicit representation of all transfer types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferSpec {
    /// Push transfer: local sources → remote destination.
    ///
    /// The local process acts as generator/sender.
    Push {
        /// Local file paths to send.
        local_sources: Vec<String>,
        /// Remote destination operand (e.g., "user@host:/path").
        remote_dest: String,
    },

    /// Pull transfer: remote sources → local destination.
    ///
    /// The local process acts as receiver.
    Pull {
        /// Remote source operand(s) (e.g., "user@host:/path").
        remote_sources: RemoteOperands,
        /// Local destination path.
        local_dest: String,
    },

    /// Proxy transfer: remote sources → remote destination (via local).
    ///
    /// The local process relays protocol messages between two remote hosts.
    Proxy {
        /// Remote source operand(s) (e.g., "user@src:/path").
        remote_sources: RemoteOperands,
        /// Remote destination operand (e.g., "user@dst:/path").
        remote_dest: String,
    },
}

impl TransferSpec {
    /// Returns the transfer role for the local process.
    #[inline]
    #[must_use]
    pub fn role(&self) -> RemoteRole {
        match self {
            TransferSpec::Push { .. } => RemoteRole::Sender,
            TransferSpec::Pull { .. } => RemoteRole::Receiver,
            TransferSpec::Proxy { .. } => RemoteRole::Proxy,
        }
    }
}

/// Result of building a remote invocation with secluded-args support.
///
/// When secluded-args is enabled, the command-line arguments are minimal
/// (just `rsync --server -s`) and the full argument list is provided
/// separately for transmission over stdin after SSH connection.
#[derive(Debug)]
pub struct SecludedInvocation {
    /// Arguments to place on the SSH command line (minimal when secluded-args).
    pub command_line_args: Vec<OsString>,
    /// Arguments to send over stdin (non-empty only when secluded-args is active).
    /// Each string is sent null-separated with an empty-string terminator.
    pub stdin_args: Vec<String>,
}

/// Builder for constructing remote rsync `--server` invocation arguments.
///
/// This builder translates client configuration options into the compact flag
/// string format expected by `rsync --server`. The resulting argument vector
/// follows upstream rsync's `server_options()` format.
///
/// # Format
///
/// **Pull (local=receiver, remote=sender):**
/// ```text
/// rsync --server --sender -flags . /remote/path
/// ```
///
/// **Push (local=sender, remote=receiver):**
/// ```text
/// rsync --server -flags . /remote/path
/// ```
///
/// The `.` is a dummy argument required by upstream rsync for compatibility.
///
/// # Secluded Args
///
/// When `--protect-args` / `-s` is enabled, the builder produces a minimal
/// command line containing only `rsync --server -s` (plus `--sender` for pull),
/// and the full argument list is provided via [`SecludedInvocation::stdin_args`]
/// for transmission over stdin after SSH connection establishment.
pub struct RemoteInvocationBuilder<'a> {
    config: &'a ClientConfig,
    role: RemoteRole,
}

impl<'a> RemoteInvocationBuilder<'a> {
    /// Creates a new builder for the specified role and client configuration.
    #[must_use]
    pub const fn new(config: &'a ClientConfig, role: RemoteRole) -> Self {
        Self { config, role }
    }

    /// Builds the complete invocation argument vector.
    ///
    /// The first element is the rsync binary name (either from `--rsync-path`
    /// or "rsync" by default), followed by "--server", optional role flags,
    /// the compact flag string, ".", and the remote path(s).
    pub fn build(&self, remote_path: &str) -> Vec<OsString> {
        self.build_with_paths(&[remote_path])
    }

    /// Builds the complete invocation argument vector with multiple remote paths.
    ///
    /// This is used for pull operations with multiple remote sources from the same host.
    pub fn build_with_paths(&self, remote_paths: &[&str]) -> Vec<OsString> {
        let mut args = Vec::new();

        // Use custom rsync path if specified, otherwise default to "rsync"
        if let Some(rsync_path) = self.config.rsync_path() {
            args.push(OsString::from(rsync_path));
        } else {
            args.push(OsString::from("rsync"));
        }

        args.extend(self.build_args_without_program(remote_paths));
        args
    }

    /// Builds an invocation with secluded-args support.
    ///
    /// When secluded args is active, the SSH command line contains only the
    /// minimal server startup arguments (`rsync --server [-s] [--sender]`),
    /// and the full argument list is returned in `stdin_args` for transmission
    /// over stdin after the SSH connection is established.
    ///
    /// When secluded args is not active, this returns the same result as
    /// `build_with_paths` with an empty `stdin_args`.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `send_protected_args()` in upstream `main.c:1119`.
    pub fn build_secluded(self, remote_paths: &[&str]) -> SecludedInvocation {
        if !self.config.protect_args().unwrap_or(false) {
            return SecludedInvocation {
                command_line_args: self.build_with_paths(remote_paths),
                stdin_args: Vec::new(),
            };
        }

        // Build the full argument list as if secluded args were off —
        // these are what we will send over stdin.
        let full_args = self.build_full_args_for_stdin(remote_paths);

        // Build the minimal command line: rsync --server [-s] [--sender]
        let mut cmd_args = Vec::new();
        if let Some(rsync_path) = self.config.rsync_path() {
            cmd_args.push(OsString::from(rsync_path));
        } else {
            cmd_args.push(OsString::from("rsync"));
        }
        cmd_args.push(OsString::from("--server"));
        if self.role == RemoteRole::Receiver {
            cmd_args.push(OsString::from("--sender"));
        }
        // The `-s` flag tells the remote server to read args from stdin.
        // upstream: options.c — protect_args flag sent as `-s` in server mode
        cmd_args.push(OsString::from("-s"));
        // Dummy argument required by upstream
        cmd_args.push(OsString::from("."));

        SecludedInvocation {
            command_line_args: cmd_args,
            stdin_args: full_args,
        }
    }

    /// Builds the full argument list for stdin transmission in secluded-args mode.
    ///
    /// This produces the same arguments as `build_with_paths()` but as `String`
    /// values suitable for null-separated transmission over stdin.
    fn build_full_args_for_stdin(&self, remote_paths: &[&str]) -> Vec<String> {
        let os_args = self.build_args_without_program(remote_paths);
        os_args
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Builds the argument list without the rsync program name.
    ///
    /// This is shared between normal `build_with_paths` and secluded-args
    /// `build_full_args_for_stdin`. The result includes `--server`, optional
    /// `--sender`, flags, capability string, `.`, and remote paths.
    fn build_args_without_program(&self, remote_paths: &[&str]) -> Vec<OsString> {
        let mut args = Vec::new();

        args.push(OsString::from("--server"));

        if self.role == RemoteRole::Receiver {
            args.push(OsString::from("--sender"));
        }

        if self.config.ignore_errors() {
            args.push(OsString::from("--ignore-errors"));
        }

        if self.config.fsync() {
            args.push(OsString::from("--fsync"));
        }

        let flags = self.build_flag_string();
        if !flags.is_empty() {
            args.push(OsString::from(flags));
        }

        // Long-form options that cannot be expressed as single-char flags.
        // Order mirrors upstream options.c server_options().
        self.append_long_form_args(&mut args);

        args.push(OsString::from("-e.LsfxCIvu"));
        args.push(OsString::from("."));

        for path in remote_paths {
            args.push(OsString::from(path));
        }

        args
    }

    /// Appends long-form `--option=value` arguments to the argument vector.
    ///
    /// These are options that upstream rsync's `server_options()` emits as separate
    /// `--key=value` tokens rather than single-character flags. The order mirrors
    /// upstream for predictable interop testing.
    fn append_long_form_args(&self, args: &mut Vec<OsString>) {
        // --delete-* timing variants
        // upstream: options.c — delete_mode forwarded as --delete-before/during/after/delay
        match self.config.delete_mode() {
            DeleteMode::Disabled => {}
            DeleteMode::Before => args.push(OsString::from("--delete-before")),
            DeleteMode::During => args.push(OsString::from("--delete-during")),
            DeleteMode::After => args.push(OsString::from("--delete-after")),
            DeleteMode::Delay => args.push(OsString::from("--delete-delay")),
        }

        if self.config.delete_excluded() {
            args.push(OsString::from("--delete-excluded"));
        }

        if self.config.force_replacements() {
            args.push(OsString::from("--force"));
        }

        // --max-delete=N
        if let Some(max) = self.config.max_delete() {
            args.push(OsString::from(format!("--max-delete={max}")));
        }

        // --max-size / --min-size
        if let Some(max) = self.config.max_file_size() {
            args.push(OsString::from(format!("--max-size={max}")));
        }
        if let Some(min) = self.config.min_file_size() {
            args.push(OsString::from(format!("--min-size={min}")));
        }

        // --modify-window=N
        if let Some(window) = self.config.modify_window() {
            args.push(OsString::from(format!("--modify-window={window}")));
        }

        // --compress-level=N
        // upstream: options.c — compress_level sent to server when explicitly set
        if let Some(level) = self.config.compression_level() {
            let numeric = compression_level_to_numeric(level);
            args.push(OsString::from(format!("--compress-level={numeric}")));
        }

        // --compress-choice=ALGO (non-default compression algorithm)
        // upstream: options.c — compress_choice forwarded when not the default zlib
        let algo = self.config.compression_algorithm();
        if algo != CompressionAlgorithm::default_algorithm() {
            args.push(OsString::from(format!("--compress-choice={}", algo.name())));
        }

        // --checksum-choice=ALGO
        // upstream: options.c — checksum_choice forwarded when not auto
        let checksum_choice = self.config.checksum_choice();
        if checksum_choice.transfer() != StrongChecksumAlgorithm::Auto
            || checksum_choice.file() != StrongChecksumAlgorithm::Auto
        {
            args.push(OsString::from(format!(
                "--checksum-choice={}",
                checksum_choice.to_argument()
            )));
        }

        // --block-size=N
        if let Some(bs) = self.config.block_size_override() {
            args.push(OsString::from(format!("--block-size={}", bs.get())));
        }

        // --timeout=N
        if let TransferTimeout::Seconds(secs) = self.config.timeout() {
            args.push(OsString::from(format!("--timeout={}", secs.get())));
        }

        // --bwlimit=N
        // upstream: options.c — bwlimit forwarded as bytes-per-second
        if let Some(bwlimit) = self.config.bandwidth_limit() {
            let mut arg = OsString::from("--bwlimit=");
            arg.push(bwlimit.fallback_argument());
            args.push(arg);
        }

        // --partial-dir=DIR
        if let Some(dir) = self.config.partial_directory() {
            let mut arg = OsString::from("--partial-dir=");
            arg.push(dir.as_os_str());
            args.push(arg);
        }

        // --temp-dir=DIR
        if let Some(dir) = self.config.temp_directory() {
            let mut arg = OsString::from("--temp-dir=");
            arg.push(dir.as_os_str());
            args.push(arg);
        }

        if self.config.inplace() {
            args.push(OsString::from("--inplace"));
        }

        if self.config.append() {
            args.push(OsString::from("--append"));
        } else if self.config.append_verify() {
            args.push(OsString::from("--append-verify"));
        }

        // --copy-unsafe-links, --safe-links, --munge-links
        if self.config.copy_unsafe_links() {
            args.push(OsString::from("--copy-unsafe-links"));
        }
        if self.config.safe_links() {
            args.push(OsString::from("--safe-links"));
        }
        if self.config.munge_links() {
            args.push(OsString::from("--munge-links"));
        }

        if self.config.size_only() {
            args.push(OsString::from("--size-only"));
        }
        if self.config.ignore_times() {
            args.push(OsString::from("--ignore-times"));
        }
        if self.config.ignore_existing() {
            args.push(OsString::from("--ignore-existing"));
        }
        if self.config.existing_only() {
            args.push(OsString::from("--existing"));
        }

        if self.config.remove_source_files() {
            args.push(OsString::from("--remove-source-files"));
        }

        if !self.config.implied_dirs() {
            args.push(OsString::from("--no-implied-dirs"));
        }

        if self.config.fake_super() {
            args.push(OsString::from("--fake-super"));
        }

        if self.config.omit_dir_times() {
            args.push(OsString::from("--omit-dir-times"));
        }
        if self.config.omit_link_times() {
            args.push(OsString::from("--omit-link-times"));
        }

        if self.config.delay_updates() {
            args.push(OsString::from("--delay-updates"));
        }

        // --backup, --backup-dir=DIR, --suffix=SUFFIX
        if self.config.backup() {
            args.push(OsString::from("--backup"));
            if let Some(dir) = self.config.backup_directory() {
                let mut arg = OsString::from("--backup-dir=");
                arg.push(dir.as_os_str());
                args.push(arg);
            }
            if let Some(suffix) = self.config.backup_suffix() {
                let mut arg = OsString::from("--suffix=");
                arg.push(suffix);
                args.push(arg);
            }
        }

        // --compare-dest, --copy-dest, --link-dest
        for ref_dir in self.config.reference_directories() {
            let flag = match ref_dir.kind() {
                ReferenceDirectoryKind::Compare => "--compare-dest=",
                ReferenceDirectoryKind::Copy => "--copy-dest=",
                ReferenceDirectoryKind::Link => "--link-dest=",
            };
            let mut arg = OsString::from(flag);
            arg.push(ref_dir.path().as_os_str());
            args.push(arg);
        }

        if self.config.copy_devices() {
            args.push(OsString::from("--copy-devices"));
        }
        if self.config.write_devices() {
            args.push(OsString::from("--write-devices"));
        }

        if self.config.open_noatime() {
            args.push(OsString::from("--open-noatime"));
        }

        if self.config.preallocate() {
            args.push(OsString::from("--preallocate"));
        }
    }

    /// Builds the compact flag string from client configuration.
    ///
    /// Format: `-logDtpre.iLsfxC` where:
    /// - Transfer flags before `.` separator
    /// - Info/debug flags after `.` separator
    fn build_flag_string(&self) -> String {
        let mut flags = String::from("-");

        // Transfer flags (order matches upstream server_options())
        if self.config.links() {
            flags.push('l');
        }
        if self.config.copy_links() {
            flags.push('L');
        }
        if self.config.copy_dirlinks() {
            flags.push('k');
        }
        if self.config.keep_dirlinks() {
            flags.push('K');
        }
        if self.config.preserve_owner() {
            flags.push('o');
        }
        if self.config.preserve_group() {
            flags.push('g');
        }
        if self.config.preserve_devices() || self.config.preserve_specials() {
            flags.push('D');
        }
        if self.config.preserve_times() {
            flags.push('t');
        }
        if self.config.preserve_atimes() {
            flags.push('U');
        }
        if self.config.preserve_permissions() {
            flags.push('p');
        }
        if self.config.preserve_executability() {
            flags.push('E');
        }
        if self.config.recursive() {
            flags.push('r');
        }
        if self.config.compress() {
            flags.push('z');
        }
        if self.config.checksum() {
            flags.push('c');
        }
        if self.config.preserve_hard_links() {
            flags.push('H');
        }
        #[cfg(all(unix, feature = "acl"))]
        if self.config.preserve_acls() {
            flags.push('A');
        }
        #[cfg(all(unix, feature = "xattr"))]
        if self.config.preserve_xattrs() {
            flags.push('X');
        }
        if self.config.numeric_ids() {
            flags.push('n');
        }
        if self.config.dry_run() {
            flags.push('n');
        }
        if self.config.delete_mode().is_enabled() || self.config.delete_excluded() {
            flags.push('d');
        }
        if self.config.whole_file() {
            flags.push('W');
        }
        if self.config.sparse() {
            flags.push('S');
        }
        if self.config.fuzzy() {
            flags.push('y');
        }
        for _ in 0..self.config.one_file_system_level() {
            flags.push('x');
        }
        if self.config.relative_paths() {
            flags.push('R');
        }
        if self.config.partial() {
            flags.push('P');
        }
        if self.config.update() {
            flags.push('u');
        }
        if self.config.preserve_crtimes() {
            flags.push('N');
        }
        if self.config.prune_empty_dirs() {
            flags.push('m');
        }
        for _ in 0..self.config.verbosity() {
            flags.push('v');
        }

        flags
    }
}

/// Converts a [`CompressionLevel`] into its numeric representation for the wire.
///
/// Upstream rsync sends the compression level as an integer in the range 0-9.
fn compression_level_to_numeric(level: compress::zlib::CompressionLevel) -> u32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => u32::from(n.get()),
    }
}

/// Parses a remote operand string into its components for validation.
///
/// Handles formats like:
/// - `host:path`
/// - `user@host:path`
/// - `user@host.example.com:path`
/// - `user@[::1]:path` (IPv6)
///
/// This is a simplified parser focused on extracting host/user for validation.
/// Full operand parsing happens in the SSH transport layer.
fn parse_remote_operand(operand: &str) -> Result<RemoteOperandParsed, ClientError> {
    let operand_str = operand.to_owned();

    // Split on first colon to separate host part from path
    let colon_pos = operand.rfind(':').ok_or_else(|| {
        invalid_argument_error(
            &format!("invalid remote operand: missing ':' in {operand}"),
            1,
        )
    })?;

    let host_part = &operand[..colon_pos];

    // Check for user@host format
    let (user, host_with_port) = if let Some(at_pos) = host_part.find('@') {
        let user = host_part[..at_pos].to_string();
        let host = &host_part[at_pos + 1..];
        (Some(user), host)
    } else {
        (None, host_part)
    };

    // For now, we don't parse port from host (would need more complex parsing for IPv6)
    // Port parsing can be added later if needed
    let host = host_with_port.to_owned();
    let port = None;

    Ok(RemoteOperandParsed {
        operand: operand_str,
        host,
        user,
        port,
    })
}

/// Validates that all remote operands are from the same host with consistent credentials.
///
/// # Errors
///
/// Returns error if:
/// - Different hosts are specified
/// - Different usernames are specified (or mixed explicit/implicit)
/// - Different ports are specified
fn validate_same_host(operands: &[RemoteOperandParsed]) -> Result<(), ClientError> {
    if operands.is_empty() {
        return Ok(());
    }

    let first = &operands[0];

    for operand in &operands[1..] {
        // Validate host consistency
        if operand.host != first.host {
            return Err(invalid_argument_error(
                &format!(
                    "all remote sources must be from the same host (found '{}' and '{}')",
                    first.host, operand.host
                ),
                1,
            ));
        }

        // Validate user consistency
        match (&operand.user, &first.user) {
            (Some(u1), Some(u2)) if u1 != u2 => {
                return Err(invalid_argument_error(
                    &format!("remote sources must use the same username (found '{u2}' and '{u1}')"),
                    1,
                ));
            }
            (Some(u), None) | (None, Some(u)) => {
                return Err(invalid_argument_error(
                    &format!("cannot mix explicit username ('{u}') with implicit username"),
                    1,
                ));
            }
            _ => {}
        }

        // Validate port consistency
        if operand.port != first.port {
            return Err(invalid_argument_error(
                "remote sources must use the same port",
                1,
            ));
        }
    }

    Ok(())
}

/// Determines the transfer type and role from source and destination operands.
///
/// Analyzes the operands to determine whether this is a push (local → remote),
/// pull (remote → local), or proxy (remote → remote) transfer.
///
/// # Arguments
///
/// * `sources` - Source operand(s)
/// * `destination` - Destination operand
///
/// # Returns
///
/// A [`TransferSpec`] describing the transfer type with all relevant operands.
///
/// # Errors
///
/// Returns error if:
/// - Neither source nor destination is remote (should use local copy)
/// - Multiple sources with different remote/local mix
/// - Multiple remote sources from different hosts, users, or ports
pub fn determine_transfer_role(
    sources: &[OsString],
    destination: &OsString,
) -> Result<TransferSpec, ClientError> {
    let dest_is_remote = operand_is_remote(destination);

    // Check if any sources are remote
    let remote_sources: Vec<_> = sources.iter().filter(|s| operand_is_remote(s)).collect();

    let has_remote_source = !remote_sources.is_empty();
    let all_sources_remote = remote_sources.len() == sources.len();

    match (has_remote_source, dest_is_remote) {
        (true, true) => {
            // Remote-to-remote: proxy between two remote hosts
            if !all_sources_remote {
                return Err(invalid_argument_error(
                    "mixing remote and local sources is not supported",
                    1,
                ));
            }

            // Parse all remote sources
            let parsed_sources: Result<Vec<_>, _> = sources
                .iter()
                .map(|s| parse_remote_operand(&s.to_string_lossy()))
                .collect();
            let parsed_sources = parsed_sources?;

            // Validate all sources are from the same host
            validate_same_host(&parsed_sources)?;

            // Build remote source operands
            let remote_sources = if sources.len() > 1 {
                RemoteOperands::Multiple(
                    sources
                        .iter()
                        .map(|s| s.to_string_lossy().to_string())
                        .collect(),
                )
            } else {
                RemoteOperands::Single(sources[0].to_string_lossy().to_string())
            };

            Ok(TransferSpec::Proxy {
                remote_sources,
                remote_dest: destination.to_string_lossy().to_string(),
            })
        }
        (false, false) => {
            // Neither is remote - should use local copy
            Err(invalid_argument_error("no remote operand found", 1))
        }
        (true, false) => {
            // Pull: remote source(s) → local destination
            if !all_sources_remote {
                return Err(invalid_argument_error(
                    "mixing remote and local sources is not supported",
                    1,
                ));
            }

            // Parse all remote sources
            let parsed_sources: Result<Vec<_>, _> = sources
                .iter()
                .map(|s| parse_remote_operand(&s.to_string_lossy()))
                .collect();
            let parsed_sources = parsed_sources?;

            // Validate all sources are from the same host
            validate_same_host(&parsed_sources)?;

            let local_dest = destination.to_string_lossy().to_string();

            // Return Multiple if > 1 source, Single otherwise
            let remote_sources = if sources.len() > 1 {
                RemoteOperands::Multiple(
                    sources
                        .iter()
                        .map(|s| s.to_string_lossy().to_string())
                        .collect(),
                )
            } else {
                RemoteOperands::Single(sources[0].to_string_lossy().to_string())
            };

            Ok(TransferSpec::Pull {
                remote_sources,
                local_dest,
            })
        }
        (false, true) => {
            // Push: local source(s) → remote destination
            let local_sources: Vec<String> = sources
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();

            Ok(TransferSpec::Push {
                local_sources,
                remote_dest: destination.to_string_lossy().to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_receiver_invocation_with_sender_flag() {
        // Pull: local is receiver → remote needs --sender (upstream options.c:2598)
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
        let args = builder.build("/remote/path");

        assert_eq!(args[0], "rsync");
        assert_eq!(args[1], "--server");
        assert_eq!(args[2], "--sender");
        let flags = args[3].to_string_lossy();
        assert!(flags.starts_with('-'), "flags should start with -: {flags}");
        assert_eq!(args[4], "-e.LsfxCIvu");
        assert_eq!(args[5], ".");
        assert_eq!(args[6], "/remote/path");
    }

    #[test]
    fn builds_sender_invocation_no_sender_flag() {
        // Push: local is sender → remote is receiver, no --sender flag
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/remote/path");

        assert_eq!(args[0], "rsync");
        assert_eq!(args[1], "--server");
        // No --sender flag for push - flags come next
        let flags = args[2].to_string_lossy();
        assert!(flags.starts_with('-'), "flags should start with -: {flags}");
        assert_eq!(args[3], "-e.LsfxCIvu");
        assert_eq!(args[4], ".");
        assert_eq!(args[5], "/remote/path");
    }

    #[test]
    fn includes_recursive_flag_when_enabled() {
        let config = ClientConfig::builder().recursive(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        // Sender (push): rsync --server -flags . /path — flags at index 2
        let flags = args[2].to_string_lossy();
        assert!(flags.contains('r'), "expected 'r' in flags: {flags}");
    }

    #[test]
    fn includes_multiple_preservation_flags() {
        let config = ClientConfig::builder()
            .times(true)
            .permissions(true)
            .owner(true)
            .group(true)
            .build();

        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        // Sender (push): rsync --server -flags . /path — flags at index 2
        let flags = args[2].to_string_lossy();
        assert!(flags.contains('t'), "expected 't' in flags: {flags}");
        assert!(flags.contains('p'), "expected 'p' in flags: {flags}");
        assert!(flags.contains('o'), "expected 'o' in flags: {flags}");
        assert!(flags.contains('g'), "expected 'g' in flags: {flags}");
    }

    #[test]
    fn includes_compress_flag() {
        let config = ClientConfig::builder().compress(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        // Sender (push): rsync --server -flags . /path — flags at index 2
        let flags = args[2].to_string_lossy();
        assert!(flags.contains('z'), "expected 'z' in flags: {flags}");
    }

    #[test]
    fn detects_push_when_destination_remote() {
        let sources = vec![OsString::from("local.txt")];
        let destination = OsString::from("user@host:/remote.txt");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.role(), RemoteRole::Sender);
        match result {
            TransferSpec::Push {
                local_sources,
                remote_dest,
            } => {
                assert_eq!(local_sources, vec!["local.txt"]);
                assert_eq!(remote_dest, "user@host:/remote.txt");
            }
            _ => panic!("Expected Push transfer"),
        }
    }

    #[test]
    fn detects_pull_when_source_remote() {
        let sources = vec![OsString::from("user@host:/remote.txt")];
        let destination = OsString::from("local.txt");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.role(), RemoteRole::Receiver);
        match result {
            TransferSpec::Pull {
                remote_sources,
                local_dest,
            } => {
                assert_eq!(local_dest, "local.txt");
                assert_eq!(
                    remote_sources,
                    RemoteOperands::Single("user@host:/remote.txt".to_owned())
                );
            }
            _ => panic!("Expected Pull transfer"),
        }
    }

    #[test]
    fn detects_push_with_multiple_sources() {
        let sources = vec![OsString::from("file1.txt"), OsString::from("file2.txt")];
        let destination = OsString::from("host:/dest/");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.role(), RemoteRole::Sender);
        match result {
            TransferSpec::Push {
                local_sources,
                remote_dest,
            } => {
                assert_eq!(local_sources, vec!["file1.txt", "file2.txt"]);
                assert_eq!(remote_dest, "host:/dest/");
            }
            _ => panic!("Expected Push transfer"),
        }
    }

    #[test]
    fn detects_proxy_when_both_remote() {
        let sources = vec![OsString::from("host1:/file")];
        let destination = OsString::from("host2:/file");

        let result = determine_transfer_role(&sources, &destination).unwrap();
        assert_eq!(result.role(), RemoteRole::Proxy);
        match result {
            TransferSpec::Proxy {
                remote_sources,
                remote_dest,
            } => {
                assert_eq!(
                    remote_sources,
                    RemoteOperands::Single("host1:/file".to_owned())
                );
                assert_eq!(remote_dest, "host2:/file");
            }
            _ => panic!("Expected Proxy transfer"),
        }
    }

    #[test]
    fn rejects_neither_remote() {
        let sources = vec![OsString::from("local1.txt")];
        let destination = OsString::from("local2.txt");

        let result = determine_transfer_role(&sources, &destination);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_mixed_remote_and_local_sources() {
        let sources = vec![
            OsString::from("local.txt"),
            OsString::from("host:/remote.txt"),
        ];
        let destination = OsString::from("dest/");

        let result = determine_transfer_role(&sources, &destination);
        assert!(result.is_err());
    }

    #[test]
    fn accepts_multiple_remote_sources_same_host() {
        let sources = vec![OsString::from("host:/file1"), OsString::from("host:/file2")];
        let destination = OsString::from("dest/");

        let result = determine_transfer_role(&sources, &destination).unwrap();
        assert_eq!(result.role(), RemoteRole::Receiver);
        match result {
            TransferSpec::Pull {
                remote_sources,
                local_dest,
            } => {
                assert_eq!(local_dest, "dest/");
                assert_eq!(
                    remote_sources,
                    RemoteOperands::Multiple(vec![
                        "host:/file1".to_owned(),
                        "host:/file2".to_owned()
                    ])
                );
            }
            _ => panic!("Expected Pull transfer"),
        }
    }

    #[test]
    fn rejects_multiple_remote_sources_different_hosts() {
        let sources = vec![
            OsString::from("host1:/file1"),
            OsString::from("host2:/file2"),
        ];
        let destination = OsString::from("dest/");

        let result = determine_transfer_role(&sources, &destination);
        assert!(result.is_err());
    }

    #[test]
    fn includes_ignore_errors_flag_when_set() {
        let config = ClientConfig::builder().ignore_errors(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        // --ignore-errors should appear after --server
        assert!(
            args.iter().any(|a| a == "--ignore-errors"),
            "expected --ignore-errors in args: {args:?}"
        );
    }

    #[test]
    fn omits_ignore_errors_flag_when_not_set() {
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        // --ignore-errors should not appear
        assert!(
            !args.iter().any(|a| a == "--ignore-errors"),
            "unexpected --ignore-errors in args: {args:?}"
        );
    }

    #[test]
    fn includes_fsync_flag_when_set() {
        let config = ClientConfig::builder().fsync(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
        let args = builder.build("/path");

        // --fsync should appear after --server
        assert!(
            args.iter().any(|a| a == "--fsync"),
            "expected --fsync in args: {args:?}"
        );
    }

    #[test]
    fn omits_fsync_flag_when_not_set() {
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
        let args = builder.build("/path");

        // --fsync should not appear
        assert!(
            !args.iter().any(|a| a == "--fsync"),
            "unexpected --fsync in args: {args:?}"
        );
    }

    /// Allowlist of long-form argument prefixes that upstream rsync 3.x recognises
    /// in `--server` mode.  Any long flag emitted by `RemoteInvocationBuilder`
    /// whose prefix is NOT on this list would break interop with stock rsync.
    /// Arguments with `=value` suffixes are matched by prefix (e.g. `--timeout=30`
    /// matches the `--timeout` prefix).
    const UPSTREAM_SERVER_LONG_ARGS: &[&str] = &[
        "--server",
        "--sender",
        "--ignore-errors",
        "--fsync",
        "--delete-before",
        "--delete-during",
        "--delete-after",
        "--delete-delay",
        "--delete-excluded",
        "--force",
        "--max-delete",
        "--max-size",
        "--min-size",
        "--modify-window",
        "--compress-level",
        "--compress-choice",
        "--checksum-choice",
        "--block-size",
        "--timeout",
        "--bwlimit",
        "--partial-dir",
        "--temp-dir",
        "--inplace",
        "--append",
        "--append-verify",
        "--copy-unsafe-links",
        "--safe-links",
        "--munge-links",
        "--size-only",
        "--ignore-times",
        "--ignore-existing",
        "--existing",
        "--remove-source-files",
        "--no-implied-dirs",
        "--fake-super",
        "--omit-dir-times",
        "--omit-link-times",
        "--delay-updates",
        "--backup",
        "--backup-dir",
        "--suffix",
        "--compare-dest",
        "--copy-dest",
        "--link-dest",
        "--copy-devices",
        "--write-devices",
        "--open-noatime",
        "--preallocate",
    ];

    /// Returns whether a long-form argument matches one of the upstream allowlist
    /// entries, accounting for `=value` suffixes.
    fn is_upstream_compatible_long_arg(arg: &str) -> bool {
        UPSTREAM_SERVER_LONG_ARGS
            .iter()
            .any(|&allowed| arg == allowed || arg.starts_with(&format!("{allowed}=")))
    }

    /// Validate that every argument sent to the remote server is compatible
    /// with upstream rsync's `--server` mode.  This catches regressions where
    /// an oc-rsync-only flag accidentally leaks into the remote invocation.
    #[test]
    fn remote_invocation_only_sends_upstream_compatible_args() {
        // Build a config with every oc-rsync extension enabled so we can
        // verify none of them leak into the remote argument vector.
        let config = ClientConfig::builder()
            .fsync(true)
            .ignore_errors(true)
            .recursive(true)
            .links(true)
            .owner(true)
            .group(true)
            .times(true)
            .permissions(true)
            .compress(true)
            .checksum(true)
            .sparse(true)
            .build();

        for role in [RemoteRole::Sender, RemoteRole::Receiver] {
            let builder = RemoteInvocationBuilder::new(&config, role);
            let args = builder.build("/path");

            for arg in &args {
                let s = arg.to_string_lossy();

                // Skip the program name, the "." placeholder, and remote paths
                if s == "rsync" || s == "." || !s.starts_with('-') {
                    continue;
                }

                // Compact flag strings (single dash, not "--") are upstream-compatible
                // by construction — they use the same single-char flags as upstream.
                if s.starts_with('-') && !s.starts_with("--") {
                    continue;
                }

                // Long-form args must be on the upstream allowlist
                assert!(
                    is_upstream_compatible_long_arg(&s),
                    "remote invocation contains non-upstream long arg {s:?} \
                     (role={role:?}, full args={args:?}). \
                     If this is intentional, add it to UPSTREAM_SERVER_LONG_ARGS \
                     after verifying upstream rsync accepts it."
                );
            }
        }
    }

    // ==================== new option forwarding tests ====================

    #[test]
    fn includes_delete_before_long_arg() {
        let config = ClientConfig::builder().delete_before(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--delete-before"),
            "expected --delete-before in args: {args:?}"
        );
    }

    #[test]
    fn includes_delete_after_long_arg() {
        let config = ClientConfig::builder().delete_after(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--delete-after"),
            "expected --delete-after in args: {args:?}"
        );
    }

    #[test]
    fn includes_delete_excluded_long_arg() {
        let config = ClientConfig::builder().delete_excluded(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--delete-excluded"),
            "expected --delete-excluded in args: {args:?}"
        );
    }

    #[test]
    fn includes_timeout_long_arg() {
        use std::num::NonZeroU64;
        let config = ClientConfig::builder()
            .timeout(TransferTimeout::Seconds(NonZeroU64::new(30).unwrap()))
            .build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--timeout=30"),
            "expected --timeout=30 in args: {args:?}"
        );
    }

    #[test]
    fn includes_bwlimit_long_arg() {
        use crate::client::config::BandwidthLimit;
        use std::num::NonZeroU64;
        let config = ClientConfig::builder()
            .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
                NonZeroU64::new(1024).unwrap(),
            )))
            .build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter()
                .any(|a| a.to_string_lossy().starts_with("--bwlimit=")),
            "expected --bwlimit=... in args: {args:?}"
        );
    }

    #[test]
    fn includes_inplace_long_arg() {
        let config = ClientConfig::builder().inplace(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--inplace"),
            "expected --inplace in args: {args:?}"
        );
    }

    #[test]
    fn includes_partial_dir_long_arg() {
        let config = ClientConfig::builder()
            .partial_directory(Some(".rsync-partial"))
            .build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter()
                .any(|a| a.to_string_lossy() == "--partial-dir=.rsync-partial"),
            "expected --partial-dir=.rsync-partial in args: {args:?}"
        );
    }

    #[test]
    fn includes_checksum_choice_long_arg() {
        use crate::client::config::StrongChecksumChoice;
        let choice = StrongChecksumChoice::parse("md5").unwrap();
        let config = ClientConfig::builder().checksum_choice(choice).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter()
                .any(|a| a.to_string_lossy().starts_with("--checksum-choice=")),
            "expected --checksum-choice=... in args: {args:?}"
        );
    }

    #[test]
    fn includes_copy_links_flag() {
        let config = ClientConfig::builder().copy_links(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[2].to_string_lossy();
        assert!(flags.contains('L'), "expected 'L' in flags: {flags}");
    }

    #[test]
    fn includes_keep_dirlinks_flag() {
        let config = ClientConfig::builder().keep_dirlinks(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[2].to_string_lossy();
        assert!(flags.contains('K'), "expected 'K' in flags: {flags}");
    }

    #[test]
    fn includes_executability_flag() {
        let config = ClientConfig::builder().executability(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[2].to_string_lossy();
        assert!(flags.contains('E'), "expected 'E' in flags: {flags}");
    }

    #[test]
    fn includes_fuzzy_flag() {
        let config = ClientConfig::builder().fuzzy(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[2].to_string_lossy();
        assert!(flags.contains('y'), "expected 'y' in flags: {flags}");
    }

    #[test]
    fn includes_prune_empty_dirs_flag() {
        let config = ClientConfig::builder().prune_empty_dirs(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[2].to_string_lossy();
        assert!(flags.contains('m'), "expected 'm' in flags: {flags}");
    }

    #[test]
    fn includes_verbosity_flags() {
        let config = ClientConfig::builder().verbosity(3).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[2].to_string_lossy();
        let v_count = flags.chars().filter(|c| *c == 'v').count();
        assert_eq!(v_count, 3, "expected 3 'v' chars in flags: {flags}");
    }

    #[test]
    fn includes_backup_related_args() {
        let config = ClientConfig::builder()
            .backup(true)
            .backup_directory(Some("/backup"))
            .backup_suffix(Some(".bak"))
            .build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--backup"),
            "expected --backup in args: {args:?}"
        );
        assert!(
            args.iter()
                .any(|a| a.to_string_lossy() == "--backup-dir=/backup"),
            "expected --backup-dir=/backup in args: {args:?}"
        );
        assert!(
            args.iter().any(|a| a.to_string_lossy() == "--suffix=.bak"),
            "expected --suffix=.bak in args: {args:?}"
        );
    }

    #[test]
    fn includes_link_dest_via_reference_directories() {
        let config = ClientConfig::builder().link_destination("/prev").build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter()
                .any(|a| a.to_string_lossy() == "--link-dest=/prev"),
            "expected --link-dest=/prev in args: {args:?}"
        );
    }

    #[test]
    fn includes_fake_super_long_arg() {
        let config = ClientConfig::builder().fake_super(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--fake-super"),
            "expected --fake-super in args: {args:?}"
        );
    }

    #[test]
    fn includes_delay_updates_long_arg() {
        let config = ClientConfig::builder().delay_updates(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--delay-updates"),
            "expected --delay-updates in args: {args:?}"
        );
    }

    #[test]
    fn includes_remove_source_files_long_arg() {
        let config = ClientConfig::builder().remove_source_files(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--remove-source-files"),
            "expected --remove-source-files in args: {args:?}"
        );
    }

    #[test]
    fn includes_size_only_long_arg() {
        let config = ClientConfig::builder().size_only(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--size-only"),
            "expected --size-only in args: {args:?}"
        );
    }

    #[test]
    fn includes_no_implied_dirs_when_disabled() {
        let config = ClientConfig::builder().implied_dirs(false).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            args.iter().any(|a| a == "--no-implied-dirs"),
            "expected --no-implied-dirs in args: {args:?}"
        );
    }

    #[test]
    fn omits_no_implied_dirs_when_default() {
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        assert!(
            !args.iter().any(|a| a == "--no-implied-dirs"),
            "unexpected --no-implied-dirs in args: {args:?}"
        );
    }

    #[test]
    fn includes_dry_run_flag() {
        let config = ClientConfig::builder().dry_run(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[2].to_string_lossy();
        assert!(flags.contains('n'), "expected 'n' in flags: {flags}");
    }

    // ==================== secluded-args tests ====================

    #[test]
    fn secluded_invocation_disabled_returns_normal_args() {
        let config = ClientConfig::builder().protect_args(None).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let secluded = builder.build_secluded(&["/path"]);

        // When secluded-args is not enabled, stdin_args should be empty
        assert!(
            secluded.stdin_args.is_empty(),
            "stdin_args should be empty when protect_args is off"
        );
        // command_line_args should contain the full invocation
        assert!(
            secluded.command_line_args.iter().any(|a| a == "/path"),
            "command_line_args should contain the remote path"
        );
    }

    #[test]
    fn secluded_invocation_enabled_produces_minimal_command_line() {
        let config = ClientConfig::builder().protect_args(Some(true)).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let secluded = builder.build_secluded(&["/path/to/files"]);

        // Command line should be minimal
        let cmd_strs: Vec<String> = secluded
            .command_line_args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(cmd_strs.contains(&"rsync".to_owned()));
        assert!(cmd_strs.contains(&"--server".to_owned()));
        assert!(cmd_strs.contains(&"-s".to_owned()));
        assert!(cmd_strs.contains(&".".to_owned()));

        // Command line should NOT contain the remote path
        assert!(
            !cmd_strs.contains(&"/path/to/files".to_owned()),
            "command line should not contain remote path in secluded mode"
        );

        // stdin_args should contain the full arguments
        assert!(
            !secluded.stdin_args.is_empty(),
            "stdin_args should not be empty when protect_args is on"
        );
        assert!(
            secluded.stdin_args.iter().any(|a| a == "/path/to/files"),
            "stdin_args should contain the remote path"
        );
    }

    #[test]
    fn secluded_invocation_pull_includes_sender_flag() {
        let config = ClientConfig::builder().protect_args(Some(true)).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
        let secluded = builder.build_secluded(&["/remote/src"]);

        let cmd_strs: Vec<String> = secluded
            .command_line_args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(
            cmd_strs.contains(&"--sender".to_owned()),
            "pull secluded invocation should include --sender on command line"
        );
        assert!(
            cmd_strs.contains(&"-s".to_owned()),
            "secluded invocation should include -s flag"
        );

        // stdin_args should also include --sender
        assert!(
            secluded.stdin_args.iter().any(|a| a == "--sender"),
            "stdin_args should include --sender for pull"
        );
    }

    #[test]
    fn secluded_invocation_stdin_args_contain_flag_string() {
        let config = ClientConfig::builder()
            .protect_args(Some(true))
            .recursive(true)
            .times(true)
            .build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let secluded = builder.build_secluded(&["/data"]);

        // stdin_args should contain the flag string
        let has_flags = secluded
            .stdin_args
            .iter()
            .any(|a| a.starts_with('-') && a.contains('r') && a.contains('t'));
        assert!(
            has_flags,
            "stdin_args should contain flag string with 'r' and 't': {:?}",
            secluded.stdin_args
        );
    }

    #[test]
    fn secluded_invocation_explicitly_disabled_returns_normal() {
        let config = ClientConfig::builder().protect_args(Some(false)).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let secluded = builder.build_secluded(&["/path"]);

        assert!(
            secluded.stdin_args.is_empty(),
            "stdin_args should be empty when protect_args is explicitly false"
        );
    }

    // ==================== comprehensive flag forwarding tests ====================
    //
    // These tests verify that every CLI flag supported by the builder is correctly
    // forwarded to the remote server invocation. Each test constructs a config with
    // a specific flag, builds the invocation, and asserts the expected argument
    // appears in the output.

    /// Helper: builds a push (Sender) invocation and returns the args vector.
    fn build_sender_args(config: &ClientConfig) -> Vec<String> {
        let builder = RemoteInvocationBuilder::new(config, RemoteRole::Sender);
        builder
            .build("/path")
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Helper: finds the compact flag string in an args vector.
    ///
    /// The flag string starts with `-` but not `--`, and is not the capability
    /// string (`-e.LsfxCIvu`). This handles variable positioning caused by
    /// `--ignore-errors` and `--fsync` appearing before the flag string.
    fn find_flag_string(args: &[String]) -> &str {
        args.iter()
            .find(|a| a.starts_with('-') && !a.starts_with("--") && !a.starts_with("-e."))
            .map(|s| s.as_str())
            .expect("flag string not found in args")
    }

    /// Helper: extracts the compact flag string from a push (Sender) invocation.
    fn sender_flag_string(config: &ClientConfig) -> String {
        let args = build_sender_args(config);
        find_flag_string(&args).to_owned()
    }

    /// Helper: builds a pull (Receiver) invocation and returns the args vector.
    fn build_receiver_args(config: &ClientConfig) -> Vec<String> {
        let builder = RemoteInvocationBuilder::new(config, RemoteRole::Receiver);
        builder
            .build("/path")
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Helper: extracts the compact flag string from a pull (Receiver) invocation.
    fn receiver_flag_string(config: &ClientConfig) -> String {
        let args = build_receiver_args(config);
        find_flag_string(&args).to_owned()
    }

    // ---------- default config produces minimal output ----------

    #[test]
    fn default_config_produces_expected_flags() {
        // ClientConfig::builder() sets recursive=true by default, and
        // whole_file() returns true when the raw option is None (auto-detect).
        let config = ClientConfig::builder().build();
        let flags = sender_flag_string(&config);
        assert!(
            flags.contains('r'),
            "default builder enables recursive: {flags}"
        );
        assert!(
            flags.contains('W'),
            "default whole_file is auto (true): {flags}"
        );
    }

    #[test]
    fn default_config_has_no_long_form_args() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        for arg in &args {
            if arg.starts_with("--") && arg != "--server" {
                panic!("default config should not emit long-form arg: {arg}");
            }
        }
    }

    // ---------- archive mode composite test (-a = -rlptgoD) ----------

    #[test]
    fn archive_mode_flags_all_present() {
        // -a is equivalent to -rlptgoD
        let config = ClientConfig::builder()
            .recursive(true)
            .links(true)
            .permissions(true)
            .times(true)
            .group(true)
            .owner(true)
            .devices(true)
            .specials(true)
            .build();

        let flags = sender_flag_string(&config);
        assert!(flags.contains('r'), "archive: missing 'r' in {flags}");
        assert!(flags.contains('l'), "archive: missing 'l' in {flags}");
        assert!(flags.contains('p'), "archive: missing 'p' in {flags}");
        assert!(flags.contains('t'), "archive: missing 't' in {flags}");
        assert!(flags.contains('g'), "archive: missing 'g' in {flags}");
        assert!(flags.contains('o'), "archive: missing 'o' in {flags}");
        assert!(flags.contains('D'), "archive: missing 'D' in {flags}");
    }

    // ---------- individual short-flag forwarding ----------

    #[test]
    fn includes_links_flag() {
        let config = ClientConfig::builder().links(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('l'), "expected 'l' in flags: {flags}");
    }

    #[test]
    fn includes_copy_dirlinks_flag() {
        let config = ClientConfig::builder().copy_dirlinks(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('k'), "expected 'k' in flags: {flags}");
    }

    #[test]
    fn includes_devices_flag() {
        let config = ClientConfig::builder().devices(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('D'), "expected 'D' in flags: {flags}");
    }

    #[test]
    fn includes_specials_flag() {
        let config = ClientConfig::builder().specials(true).build();
        let flags = sender_flag_string(&config);
        assert!(
            flags.contains('D'),
            "expected 'D' for specials in flags: {flags}"
        );
    }

    #[test]
    fn devices_and_specials_produce_single_d_flag() {
        let config = ClientConfig::builder().devices(true).specials(true).build();
        let flags = sender_flag_string(&config);
        let d_count = flags.chars().filter(|c| *c == 'D').count();
        assert_eq!(
            d_count, 1,
            "devices+specials should produce single 'D', got {d_count} in {flags}"
        );
    }

    #[test]
    fn includes_atimes_flag() {
        let config = ClientConfig::builder().atimes(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('U'), "expected 'U' in flags: {flags}");
    }

    #[test]
    fn includes_hard_links_flag() {
        let config = ClientConfig::builder().hard_links(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('H'), "expected 'H' in flags: {flags}");
    }

    #[test]
    fn includes_numeric_ids_flag() {
        let config = ClientConfig::builder().numeric_ids(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('n'), "expected 'n' in flags: {flags}");
    }

    #[test]
    fn includes_whole_file_flag() {
        let config = ClientConfig::builder().whole_file(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('W'), "expected 'W' in flags: {flags}");
    }

    #[test]
    fn includes_one_file_system_flag() {
        let config = ClientConfig::builder().one_file_system(1).build();
        let flags = sender_flag_string(&config);
        let x_count = flags.chars().filter(|c| *c == 'x').count();
        assert_eq!(x_count, 1, "expected 1 'x' in flags: {flags}");
    }

    #[test]
    fn includes_double_one_file_system_flag() {
        let config = ClientConfig::builder().one_file_system(2).build();
        let flags = sender_flag_string(&config);
        let x_count = flags.chars().filter(|c| *c == 'x').count();
        assert_eq!(x_count, 2, "expected 2 'x' in flags: {flags}");
    }

    #[test]
    fn includes_relative_paths_flag() {
        let config = ClientConfig::builder().relative_paths(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('R'), "expected 'R' in flags: {flags}");
    }

    #[test]
    fn includes_partial_flag() {
        let config = ClientConfig::builder().partial(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('P'), "expected 'P' in flags: {flags}");
    }

    #[test]
    fn includes_update_flag() {
        let config = ClientConfig::builder().update(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('u'), "expected 'u' in flags: {flags}");
    }

    #[test]
    fn includes_crtimes_flag() {
        let config = ClientConfig::builder().crtimes(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('N'), "expected 'N' in flags: {flags}");
    }

    #[test]
    fn includes_checksum_flag() {
        let config = ClientConfig::builder().checksum(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('c'), "expected 'c' in flags: {flags}");
    }

    #[test]
    fn includes_sparse_flag() {
        let config = ClientConfig::builder().sparse(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('S'), "expected 'S' in flags: {flags}");
    }

    #[test]
    fn delete_mode_sets_d_flag_in_flag_string() {
        let config = ClientConfig::builder().delete_before(true).build();
        let flags = sender_flag_string(&config);
        assert!(
            flags.contains('d'),
            "delete mode should set 'd' in flag string: {flags}"
        );
    }

    #[test]
    fn delete_excluded_sets_d_flag_in_flag_string() {
        let config = ClientConfig::builder().delete_excluded(true).build();
        let flags = sender_flag_string(&config);
        assert!(
            flags.contains('d'),
            "delete_excluded should set 'd' in flag string: {flags}"
        );
    }

    // ---------- ACL and xattr flags (feature-gated) ----------

    #[cfg(all(unix, feature = "acl"))]
    #[test]
    fn includes_acl_flag() {
        let config = ClientConfig::builder().acls(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('A'), "expected 'A' in flags: {flags}");
    }

    #[cfg(all(unix, feature = "xattr"))]
    #[test]
    fn includes_xattr_flag() {
        let config = ClientConfig::builder().xattrs(true).build();
        let flags = sender_flag_string(&config);
        assert!(flags.contains('X'), "expected 'X' in flags: {flags}");
    }

    // ---------- long-form argument forwarding ----------

    #[test]
    fn includes_delete_during_long_arg() {
        let config = ClientConfig::builder().delete_during().build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--delete-during"),
            "expected --delete-during in args: {args:?}"
        );
    }

    #[test]
    fn includes_delete_delay_long_arg() {
        let config = ClientConfig::builder().delete_delay(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--delete-delay"),
            "expected --delete-delay in args: {args:?}"
        );
    }

    #[test]
    fn includes_force_long_arg() {
        let config = ClientConfig::builder().force_replacements(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--force"),
            "expected --force in args: {args:?}"
        );
    }

    #[test]
    fn includes_max_delete_long_arg() {
        let config = ClientConfig::builder().max_delete(Some(100)).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--max-delete=100"),
            "expected --max-delete=100 in args: {args:?}"
        );
    }

    #[test]
    fn includes_max_size_long_arg() {
        let config = ClientConfig::builder().max_file_size(Some(1048576)).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--max-size=1048576"),
            "expected --max-size=1048576 in args: {args:?}"
        );
    }

    #[test]
    fn includes_min_size_long_arg() {
        let config = ClientConfig::builder().min_file_size(Some(1024)).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--min-size=1024"),
            "expected --min-size=1024 in args: {args:?}"
        );
    }

    #[test]
    fn includes_modify_window_long_arg() {
        let config = ClientConfig::builder().modify_window(Some(2)).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--modify-window=2"),
            "expected --modify-window=2 in args: {args:?}"
        );
    }

    #[test]
    fn includes_compress_level_long_arg() {
        let config = ClientConfig::builder()
            .compress(true)
            .compression_level(Some(compress::zlib::CompressionLevel::Best))
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--compress-level=9"),
            "expected --compress-level=9 in args: {args:?}"
        );
    }

    #[test]
    fn includes_compress_level_fast() {
        let config = ClientConfig::builder()
            .compress(true)
            .compression_level(Some(compress::zlib::CompressionLevel::Fast))
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--compress-level=1"),
            "expected --compress-level=1 in args: {args:?}"
        );
    }

    #[test]
    fn includes_compress_level_default() {
        let config = ClientConfig::builder()
            .compress(true)
            .compression_level(Some(compress::zlib::CompressionLevel::Default))
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--compress-level=6"),
            "expected --compress-level=6 in args: {args:?}"
        );
    }

    #[test]
    fn includes_compress_choice_zstd() {
        let config = ClientConfig::builder()
            .compression_algorithm(CompressionAlgorithm::Zstd)
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter()
                .any(|a| a.starts_with("--compress-choice=") && a.contains("zstd")),
            "expected --compress-choice=zstd in args: {args:?}"
        );
    }

    #[test]
    fn omits_compress_choice_for_default_algorithm() {
        let config = ClientConfig::builder()
            .compression_algorithm(CompressionAlgorithm::default_algorithm())
            .build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--compress-choice=")),
            "should not emit --compress-choice for default algorithm: {args:?}"
        );
    }

    #[test]
    fn includes_block_size_long_arg() {
        use std::num::NonZeroU32;
        let config = ClientConfig::builder()
            .block_size_override(Some(NonZeroU32::new(8192).unwrap()))
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--block-size=8192"),
            "expected --block-size=8192 in args: {args:?}"
        );
    }

    #[test]
    fn includes_temp_dir_long_arg() {
        let config = ClientConfig::builder()
            .temp_directory(Some("/tmp/staging"))
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--temp-dir=/tmp/staging"),
            "expected --temp-dir=/tmp/staging in args: {args:?}"
        );
    }

    #[test]
    fn includes_append_long_arg() {
        let config = ClientConfig::builder().append(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--append"),
            "expected --append in args: {args:?}"
        );
    }

    #[test]
    fn append_verify_via_builder_emits_append() {
        // The builder's append_verify(true) sets append=true internally,
        // so the invocation emits --append (the append() check comes first).
        // This mirrors upstream behavior where --append-verify implies --append.
        let config = ClientConfig::builder().append_verify(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--append"),
            "append_verify via builder should produce --append: {args:?}"
        );
    }

    #[test]
    fn append_verify_emits_append_because_builder_sets_both() {
        // The builder's append_verify(true) sets both append=true and
        // append_verify=true. The invocation code checks append() first,
        // so --append is emitted. To get --append-verify alone, one would
        // need to set append_verify without append (not possible via builder).
        let config = ClientConfig::builder().append_verify(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--append"),
            "append_verify via builder should emit --append: {args:?}"
        );
    }

    #[test]
    fn includes_copy_unsafe_links_long_arg() {
        let config = ClientConfig::builder().copy_unsafe_links(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--copy-unsafe-links"),
            "expected --copy-unsafe-links in args: {args:?}"
        );
    }

    #[test]
    fn includes_safe_links_long_arg() {
        let config = ClientConfig::builder().safe_links(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--safe-links"),
            "expected --safe-links in args: {args:?}"
        );
    }

    #[test]
    fn includes_munge_links_long_arg() {
        let config = ClientConfig::builder().munge_links(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--munge-links"),
            "expected --munge-links in args: {args:?}"
        );
    }

    #[test]
    fn includes_ignore_times_long_arg() {
        let config = ClientConfig::builder().ignore_times(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--ignore-times"),
            "expected --ignore-times in args: {args:?}"
        );
    }

    #[test]
    fn includes_ignore_existing_long_arg() {
        let config = ClientConfig::builder().ignore_existing(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--ignore-existing"),
            "expected --ignore-existing in args: {args:?}"
        );
    }

    #[test]
    fn includes_existing_only_long_arg() {
        let config = ClientConfig::builder().existing_only(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--existing"),
            "expected --existing in args: {args:?}"
        );
    }

    #[test]
    fn includes_omit_dir_times_long_arg() {
        let config = ClientConfig::builder().omit_dir_times(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--omit-dir-times"),
            "expected --omit-dir-times in args: {args:?}"
        );
    }

    #[test]
    fn includes_omit_link_times_long_arg() {
        let config = ClientConfig::builder().omit_link_times(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--omit-link-times"),
            "expected --omit-link-times in args: {args:?}"
        );
    }

    #[test]
    fn includes_compare_dest_long_arg() {
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--compare-dest=/tmp/compare"),
            "expected --compare-dest=/tmp/compare in args: {args:?}"
        );
    }

    #[test]
    fn includes_copy_dest_long_arg() {
        let config = ClientConfig::builder()
            .copy_destination("/tmp/copy")
            .build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--copy-dest=/tmp/copy"),
            "expected --copy-dest=/tmp/copy in args: {args:?}"
        );
    }

    #[test]
    fn includes_copy_devices_long_arg() {
        let config = ClientConfig::builder().copy_devices(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--copy-devices"),
            "expected --copy-devices in args: {args:?}"
        );
    }

    #[test]
    fn includes_write_devices_long_arg() {
        let config = ClientConfig::builder().write_devices(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--write-devices"),
            "expected --write-devices in args: {args:?}"
        );
    }

    #[test]
    fn includes_open_noatime_long_arg() {
        let config = ClientConfig::builder().open_noatime(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--open-noatime"),
            "expected --open-noatime in args: {args:?}"
        );
    }

    #[test]
    fn includes_preallocate_long_arg() {
        let config = ClientConfig::builder().preallocate(true).build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--preallocate"),
            "expected --preallocate in args: {args:?}"
        );
    }

    // ---------- custom rsync path ----------

    #[test]
    fn custom_rsync_path_used_as_program_name() {
        let config = ClientConfig::builder()
            .set_rsync_path("/opt/rsync/bin/rsync")
            .build();
        let args = build_sender_args(&config);
        assert_eq!(
            args[0], "/opt/rsync/bin/rsync",
            "first arg should be custom rsync path"
        );
    }

    #[test]
    fn default_rsync_path_is_rsync() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert_eq!(args[0], "rsync", "default program name should be 'rsync'");
    }

    // ---------- capability string ----------

    #[test]
    fn capability_string_present_in_sender_args() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "-e.LsfxCIvu"),
            "expected capability string -e.LsfxCIvu in args: {args:?}"
        );
    }

    #[test]
    fn capability_string_present_in_receiver_args() {
        let config = ClientConfig::builder().build();
        let args = build_receiver_args(&config);
        assert!(
            args.iter().any(|a| a == "-e.LsfxCIvu"),
            "expected capability string -e.LsfxCIvu in args: {args:?}"
        );
    }

    // ---------- dot placeholder and path ----------

    #[test]
    fn dot_placeholder_precedes_remote_path_in_sender() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        let dot_idx = args.iter().position(|a| a == ".").unwrap();
        let path_idx = args.iter().position(|a| a == "/path").unwrap();
        assert_eq!(
            path_idx,
            dot_idx + 1,
            "remote path should immediately follow '.' placeholder"
        );
    }

    #[test]
    fn dot_placeholder_precedes_remote_path_in_receiver() {
        let config = ClientConfig::builder().build();
        let args = build_receiver_args(&config);
        let dot_idx = args.iter().position(|a| a == ".").unwrap();
        let path_idx = args.iter().position(|a| a == "/path").unwrap();
        assert_eq!(
            path_idx,
            dot_idx + 1,
            "remote path should immediately follow '.' placeholder"
        );
    }

    // ---------- multiple remote paths ----------

    #[test]
    fn build_with_multiple_paths() {
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
        let args: Vec<String> = builder
            .build_with_paths(&["/src1", "/src2", "/src3"])
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let dot_idx = args.iter().position(|a| a == ".").unwrap();
        assert_eq!(args[dot_idx + 1], "/src1");
        assert_eq!(args[dot_idx + 2], "/src2");
        assert_eq!(args[dot_idx + 3], "/src3");
    }

    // ---------- receiver-specific tests ----------

    #[test]
    fn receiver_flag_string_matches_sender_for_same_config() {
        let config = ClientConfig::builder()
            .recursive(true)
            .links(true)
            .times(true)
            .compress(true)
            .build();

        let s_flags = sender_flag_string(&config);
        let r_flags = receiver_flag_string(&config);
        assert_eq!(
            s_flags, r_flags,
            "flag strings should be identical for sender and receiver with same config"
        );
    }

    // ---------- delete mode exclusivity ----------

    #[test]
    fn only_one_delete_mode_emitted() {
        let config = ClientConfig::builder().delete_before(true).build();
        let args = build_sender_args(&config);
        let delete_args: Vec<&String> =
            args.iter().filter(|a| a.starts_with("--delete-")).collect();
        assert_eq!(
            delete_args.len(),
            1,
            "only one --delete-* arg should be emitted, got: {delete_args:?}"
        );
        assert_eq!(delete_args[0], "--delete-before");
    }

    #[test]
    fn delete_during_emits_only_during() {
        let config = ClientConfig::builder().delete_during().build();
        let args = build_sender_args(&config);
        let delete_args: Vec<&String> =
            args.iter().filter(|a| a.starts_with("--delete-")).collect();
        assert_eq!(delete_args.len(), 1);
        assert_eq!(delete_args[0], "--delete-during");
    }

    #[test]
    fn delete_delay_emits_only_delay() {
        let config = ClientConfig::builder().delete_delay(true).build();
        let args = build_sender_args(&config);
        let delete_args: Vec<&String> = args
            .iter()
            .filter(|a| a.starts_with("--delete-") && !a.starts_with("--delete-excluded"))
            .collect();
        assert_eq!(delete_args.len(), 1);
        assert_eq!(delete_args[0], "--delete-delay");
    }

    // ---------- combined flag correctness ----------

    #[test]
    fn compress_with_level_emits_both_flag_and_level() {
        let config = ClientConfig::builder()
            .compress(true)
            .compression_level(Some(compress::zlib::CompressionLevel::Best))
            .build();
        let flags = sender_flag_string(&config);
        assert!(
            flags.contains('z'),
            "expected 'z' flag for compress: {flags}"
        );
        let args = build_sender_args(&config);
        assert!(
            args.iter().any(|a| a == "--compress-level=9"),
            "expected --compress-level=9 in args: {args:?}"
        );
    }

    #[test]
    fn partial_dir_also_emits_partial_flag_string() {
        let config = ClientConfig::builder()
            .partial_directory(Some(".rsync-partial"))
            .build();
        let flags = sender_flag_string(&config);
        // partial_directory sets partial=true, which should emit 'P' flag
        assert!(
            flags.contains('P'),
            "partial_directory should also set 'P' flag: {flags}"
        );
    }

    #[test]
    fn backup_without_dir_or_suffix_emits_only_backup() {
        let config = ClientConfig::builder().backup(true).build();
        let args = build_sender_args(&config);
        assert!(args.iter().any(|a| a == "--backup"));
        assert!(!args.iter().any(|a| a.starts_with("--backup-dir=")));
        assert!(!args.iter().any(|a| a.starts_with("--suffix=")));
    }

    // ---------- omission tests: disabled flags should NOT appear ----------

    #[test]
    fn omits_compress_flag_when_disabled() {
        let config = ClientConfig::builder().build();
        let flags = sender_flag_string(&config);
        assert!(
            !flags.contains('z'),
            "should not contain 'z' when compress is off: {flags}"
        );
    }

    #[test]
    fn omits_recursive_flag_when_explicitly_disabled() {
        let config = ClientConfig::builder().recursive(false).build();
        let flags = sender_flag_string(&config);
        assert!(
            !flags.contains('r'),
            "should not contain 'r' when recursive=false: {flags}"
        );
    }

    #[test]
    fn omits_delete_long_args_when_disabled() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--delete")),
            "should not emit --delete-* when delete is disabled: {args:?}"
        );
    }

    #[test]
    fn omits_timeout_when_default() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--timeout=")),
            "should not emit --timeout= when default: {args:?}"
        );
    }

    #[test]
    fn omits_bwlimit_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--bwlimit=")),
            "should not emit --bwlimit= when none: {args:?}"
        );
    }

    #[test]
    fn omits_block_size_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--block-size=")),
            "should not emit --block-size= when none: {args:?}"
        );
    }

    #[test]
    fn omits_max_delete_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--max-delete=")),
            "should not emit --max-delete= when none: {args:?}"
        );
    }

    #[test]
    fn omits_max_size_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--max-size=")),
            "should not emit --max-size= when none: {args:?}"
        );
    }

    #[test]
    fn omits_min_size_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--min-size=")),
            "should not emit --min-size= when none: {args:?}"
        );
    }

    #[test]
    fn omits_modify_window_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--modify-window=")),
            "should not emit --modify-window= when none: {args:?}"
        );
    }

    #[test]
    fn omits_compress_level_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--compress-level=")),
            "should not emit --compress-level= when none: {args:?}"
        );
    }

    #[test]
    fn omits_checksum_choice_when_auto() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--checksum-choice=")),
            "should not emit --checksum-choice= when auto: {args:?}"
        );
    }

    #[test]
    fn omits_partial_dir_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--partial-dir=")),
            "should not emit --partial-dir= when none: {args:?}"
        );
    }

    #[test]
    fn omits_temp_dir_when_none() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--temp-dir=")),
            "should not emit --temp-dir= when none: {args:?}"
        );
    }

    #[test]
    fn omits_backup_args_when_disabled() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a == "--backup"),
            "should not emit --backup when disabled: {args:?}"
        );
    }

    #[test]
    fn omits_reference_dirs_when_empty() {
        let config = ClientConfig::builder().build();
        let args = build_sender_args(&config);
        assert!(
            !args.iter().any(|a| a.starts_with("--compare-dest=")
                || a.starts_with("--copy-dest=")
                || a.starts_with("--link-dest=")),
            "should not emit reference dir args when empty: {args:?}"
        );
    }

    // ---------- exhaustive all-flags-at-once integration test ----------

    #[test]
    fn all_flags_enabled_produces_valid_invocation() {
        use crate::client::config::{BandwidthLimit, StrongChecksumChoice};
        use std::num::{NonZeroU32, NonZeroU64};

        let choice = StrongChecksumChoice::parse("md5").unwrap();
        let config = ClientConfig::builder()
            // Short flags
            .links(true)
            .copy_links(true)
            .copy_dirlinks(true)
            .keep_dirlinks(true)
            .owner(true)
            .group(true)
            .devices(true)
            .specials(true)
            .times(true)
            .atimes(true)
            .permissions(true)
            .executability(true)
            .recursive(true)
            .compress(true)
            .checksum(true)
            .hard_links(true)
            .acls(true)
            .xattrs(true)
            .numeric_ids(true)
            .dry_run(true)
            .delete_before(true)
            .whole_file(true)
            .sparse(true)
            .fuzzy(true)
            .one_file_system(2)
            .relative_paths(true)
            .partial(true)
            .update(true)
            .crtimes(true)
            .prune_empty_dirs(true)
            .verbosity(2)
            // Long-form args
            .ignore_errors(true)
            .fsync(true)
            .delete_excluded(true)
            .force_replacements(true)
            .max_delete(Some(50))
            .max_file_size(Some(1000000))
            .min_file_size(Some(100))
            .modify_window(Some(1))
            .compression_level(Some(compress::zlib::CompressionLevel::Best))
            .compression_algorithm(CompressionAlgorithm::Zstd)
            .checksum_choice(choice)
            .block_size_override(Some(NonZeroU32::new(4096).unwrap()))
            .timeout(TransferTimeout::Seconds(NonZeroU64::new(60).unwrap()))
            .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
                NonZeroU64::new(1024).unwrap(),
            )))
            .partial_directory(Some(".partial"))
            .temp_directory(Some("/tmp/rsync"))
            .inplace(true)
            .copy_unsafe_links(true)
            .safe_links(true)
            .munge_links(true)
            .size_only(true)
            .ignore_times(true)
            .ignore_existing(true)
            .existing_only(true)
            .remove_source_files(true)
            .implied_dirs(false)
            .fake_super(true)
            .omit_dir_times(true)
            .omit_link_times(true)
            .delay_updates(true)
            .backup(true)
            .backup_directory(Some("/backup"))
            .backup_suffix(Some(".bak"))
            .link_destination("/prev")
            .compare_destination("/cmp")
            .copy_destination("/cpd")
            .copy_devices(true)
            .write_devices(true)
            .open_noatime(true)
            .preallocate(true)
            .set_rsync_path("/custom/rsync")
            .build();

        let args = build_sender_args(&config);

        // Verify structural integrity: program name, --server, flags, capability, dot, path
        assert_eq!(args[0], "/custom/rsync");
        assert_eq!(args[1], "--server");

        // Verify --ignore-errors and --fsync are present
        assert!(args.contains(&"--ignore-errors".to_owned()));
        assert!(args.contains(&"--fsync".to_owned()));

        // Verify flag string contains all expected single-char flags
        let flags = find_flag_string(&args);
        for (ch, name) in [
            ('l', "links"),
            ('L', "copy_links"),
            ('k', "copy_dirlinks"),
            ('K', "keep_dirlinks"),
            ('o', "owner"),
            ('g', "group"),
            ('D', "devices/specials"),
            ('t', "times"),
            ('U', "atimes"),
            ('p', "permissions"),
            ('E', "executability"),
            ('r', "recursive"),
            ('z', "compress"),
            ('c', "checksum"),
            ('H', "hard_links"),
            ('n', "numeric_ids/dry_run"),
            ('d', "delete"),
            ('W', "whole_file"),
            ('S', "sparse"),
            ('y', "fuzzy"),
            ('R', "relative_paths"),
            ('P', "partial"),
            ('u', "update"),
            ('N', "crtimes"),
            ('m', "prune_empty_dirs"),
        ] {
            assert!(
                flags.contains(ch),
                "all-flags test: missing '{ch}' ({name}) in flags: {flags}"
            );
        }

        // Verify 'x' count
        let x_count = flags.chars().filter(|c| *c == 'x').count();
        assert_eq!(x_count, 2, "expected 2 'x' flags for one_file_system=2");

        // Verify 'v' count
        let v_count = flags.chars().filter(|c| *c == 'v').count();
        assert_eq!(v_count, 2, "expected 2 'v' flags for verbosity=2");

        // Verify all long-form args
        let expected_long_args = [
            "--delete-before",
            "--delete-excluded",
            "--force",
            "--max-delete=50",
            "--max-size=1000000",
            "--min-size=100",
            "--modify-window=1",
            "--compress-level=9",
            "--block-size=4096",
            "--timeout=60",
            "--inplace",
            "--copy-unsafe-links",
            "--safe-links",
            "--munge-links",
            "--size-only",
            "--ignore-times",
            "--ignore-existing",
            "--existing",
            "--remove-source-files",
            "--no-implied-dirs",
            "--fake-super",
            "--omit-dir-times",
            "--omit-link-times",
            "--delay-updates",
            "--backup",
            "--copy-devices",
            "--write-devices",
            "--open-noatime",
            "--preallocate",
        ];

        for expected in expected_long_args {
            assert!(
                args.iter().any(|a| a == expected),
                "all-flags test: missing {expected} in args: {args:?}"
            );
        }

        // Verify args with values using prefix matching
        let expected_prefixed = [
            "--checksum-choice=",
            "--compress-choice=",
            "--bwlimit=",
            "--partial-dir=",
            "--temp-dir=",
            "--backup-dir=",
            "--suffix=",
            "--link-dest=",
            "--compare-dest=",
            "--copy-dest=",
        ];
        for prefix in expected_prefixed {
            assert!(
                args.iter().any(|a| a.starts_with(prefix)),
                "all-flags test: missing arg with prefix {prefix} in args: {args:?}"
            );
        }

        // Verify capability string and structural elements
        assert!(args.contains(&"-e.LsfxCIvu".to_owned()));
        assert!(args.contains(&".".to_owned()));
        assert!(args.contains(&"/path".to_owned()));
    }
}
