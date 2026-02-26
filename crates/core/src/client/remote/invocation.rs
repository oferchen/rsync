//! Remote rsync invocation builder.
//!
//! This module constructs the command-line arguments for invoking rsync in
//! `--server` mode on a remote host via SSH. The invocation format mirrors
//! upstream rsync's `server_options()` function.

use std::ffi::{OsStr, OsString};

use super::super::config::ClientConfig;
use super::super::error::{ClientError, invalid_argument_error};

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

        args.push(OsString::from("-e.LsfxCIvu"));
        args.push(OsString::from("."));

        for path in remote_paths {
            args.push(OsString::from(path));
        }

        args
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
        if self.config.delete_mode().is_enabled() || self.config.delete_excluded() {
            flags.push('d');
        }
        if self.config.whole_file() {
            flags.push('W');
        }
        if self.config.sparse() {
            flags.push('S');
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

        // Info flags after '.' separator
        // For now, we don't send info flags (upstream does this selectively)
        // flags.push('.');

        flags
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

    /// Allowlist of long-form arguments that upstream rsync 3.x recognises in
    /// `--server` mode.  Any long flag emitted by `RemoteInvocationBuilder`
    /// that is NOT on this list would break interop with stock rsync.
    const UPSTREAM_SERVER_LONG_ARGS: &[&str] =
        &["--server", "--sender", "--ignore-errors", "--fsync"];

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
                    UPSTREAM_SERVER_LONG_ARGS.contains(&s.as_ref()),
                    "remote invocation contains non-upstream long arg {s:?} \
                     (role={role:?}, full args={args:?}). \
                     If this is intentional, add it to UPSTREAM_SERVER_LONG_ARGS \
                     after verifying upstream rsync accepts it."
                );
            }
        }
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
}
