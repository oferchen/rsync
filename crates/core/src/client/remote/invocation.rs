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

/// Builder for constructing remote rsync `--server` invocation arguments.
///
/// This builder translates client configuration options into the compact flag
/// string format expected by `rsync --server`. The resulting argument vector
/// follows upstream rsync's `server_options()` format.
///
/// # Format
///
/// **Sender (pull from remote):**
/// ```text
/// rsync --server --sender -flags . /remote/path
/// ```
///
/// **Receiver (push to remote):**
/// ```text
/// rsync --server -flags . /remote/path
/// ```
///
/// The `.` is a dummy argument required by upstream rsync for compatibility.
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
    pub fn build(self, remote_path: &str) -> Vec<OsString> {
        self.build_with_paths(&[remote_path])
    }

    /// Builds the complete invocation argument vector with multiple remote paths.
    ///
    /// This is used for pull operations with multiple remote sources from the same host.
    pub fn build_with_paths(self, remote_paths: &[&str]) -> Vec<OsString> {
        let mut args = Vec::new();

        // Use custom rsync path if specified, otherwise default to "rsync"
        if let Some(rsync_path) = self.config.rsync_path() {
            args.push(OsString::from(rsync_path));
        } else {
            args.push(OsString::from("rsync"));
        }
        args.push(OsString::from("--server"));

        // Add --sender for sender role (remote is receiver)
        if self.role == RemoteRole::Sender {
            args.push(OsString::from("--sender"));
        }

        // Build compact flag string
        let flags = self.build_flag_string();
        if !flags.is_empty() {
            args.push(OsString::from(flags));
        }

        // Dummy argument required by upstream
        args.push(OsString::from("."));

        // Remote path(s) come last
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
        if self.config.preserve_acls() {
            flags.push('A');
        }
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
        if self.config.one_file_system() {
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

/// Determines the transfer role from source and destination operands.
///
/// # Arguments
///
/// * `sources` - Source operand(s)
/// * `destination` - Destination operand
///
/// # Returns
///
/// A tuple of:
/// - `RemoteRole` - Whether we're sender or receiver
/// - `Vec<String>` - Local paths (for sender role) or local destination (for receiver role)
/// - `RemoteOperands` - Remote operand(s) string(s)
///
/// # Errors
///
/// Returns error if:
/// - Both source and destination are remote (not yet supported)
/// - Neither source nor destination is remote (should use local copy)
/// - Multiple sources with different remote/local mix
/// - Multiple remote sources from different hosts, users, or ports
pub fn determine_transfer_role(
    sources: &[OsString],
    destination: &OsString,
) -> Result<(RemoteRole, Vec<String>, RemoteOperands), ClientError> {
    let dest_is_remote = operand_is_remote(destination);

    // Check if any sources are remote
    let remote_sources: Vec<_> = sources.iter().filter(|s| operand_is_remote(s)).collect();

    let has_remote_source = !remote_sources.is_empty();
    let all_sources_remote = remote_sources.len() == sources.len();

    match (has_remote_source, dest_is_remote) {
        (true, true) => {
            // Both source and dest are remote - not supported
            Err(invalid_argument_error(
                "both source and destination cannot be remote",
                1,
            ))
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

            let local_path = destination.to_string_lossy().to_string();

            // Return Multiple if > 1 source, Single otherwise
            let remote_operands = if sources.len() > 1 {
                RemoteOperands::Multiple(
                    sources
                        .iter()
                        .map(|s| s.to_string_lossy().to_string())
                        .collect(),
                )
            } else {
                RemoteOperands::Single(sources[0].to_string_lossy().to_string())
            };

            Ok((RemoteRole::Receiver, vec![local_path], remote_operands))
        }
        (false, true) => {
            // Push: local source(s) → remote destination
            let local_paths: Vec<String> = sources
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();

            let remote_operand = RemoteOperands::Single(destination.to_string_lossy().to_string());

            Ok((RemoteRole::Sender, local_paths, remote_operand))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_sender_invocation_minimal() {
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/remote/path");

        assert_eq!(args[0], "rsync");
        assert_eq!(args[1], "--server");
        assert_eq!(args[2], "--sender");
        // Default flags from ClientConfig::builder().build()
        // The builder has some defaults (recursive=true, whole_file=true)
        let flags = args[3].to_string_lossy();
        assert!(flags.starts_with('-'), "flags should start with -: {flags}");
        assert_eq!(args[4], ".");
        assert_eq!(args[5], "/remote/path");
    }

    #[test]
    fn builds_receiver_invocation_no_sender_flag() {
        let config = ClientConfig::builder().build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
        let args = builder.build("/remote/path");

        assert_eq!(args[0], "rsync");
        assert_eq!(args[1], "--server");
        // No --sender flag for receiver - flags come next
        let flags = args[2].to_string_lossy();
        assert!(flags.starts_with('-'), "flags should start with -: {flags}");
        assert_eq!(args[3], ".");
        assert_eq!(args[4], "/remote/path");
    }

    #[test]
    fn includes_recursive_flag_when_enabled() {
        let config = ClientConfig::builder().recursive(true).build();
        let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
        let args = builder.build("/path");

        let flags = args[3].to_string_lossy();
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

        let flags = args[3].to_string_lossy();
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

        let flags = args[3].to_string_lossy();
        assert!(flags.contains('z'), "expected 'z' in flags: {flags}");
    }

    #[test]
    fn detects_push_when_destination_remote() {
        let sources = vec![OsString::from("local.txt")];
        let destination = OsString::from("user@host:/remote.txt");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.0, RemoteRole::Sender);
        assert_eq!(result.1, vec!["local.txt"]);
        assert_eq!(
            result.2,
            RemoteOperands::Single("user@host:/remote.txt".to_owned())
        );
    }

    #[test]
    fn detects_pull_when_source_remote() {
        let sources = vec![OsString::from("user@host:/remote.txt")];
        let destination = OsString::from("local.txt");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.0, RemoteRole::Receiver);
        assert_eq!(result.1, vec!["local.txt"]);
        assert_eq!(
            result.2,
            RemoteOperands::Single("user@host:/remote.txt".to_owned())
        );
    }

    #[test]
    fn detects_push_with_multiple_sources() {
        let sources = vec![OsString::from("file1.txt"), OsString::from("file2.txt")];
        let destination = OsString::from("host:/dest/");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.0, RemoteRole::Sender);
        assert_eq!(result.1, vec!["file1.txt", "file2.txt"]);
        assert_eq!(result.2, RemoteOperands::Single("host:/dest/".to_owned()));
    }

    #[test]
    fn rejects_both_remote() {
        let sources = vec![OsString::from("host1:/file")];
        let destination = OsString::from("host2:/file");

        let result = determine_transfer_role(&sources, &destination);
        assert!(result.is_err());
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
        assert_eq!(result.0, RemoteRole::Receiver);
        assert_eq!(result.1, vec!["dest/"]);
        assert_eq!(
            result.2,
            RemoteOperands::Multiple(vec!["host:/file1".to_owned(), "host:/file2".to_owned()])
        );
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
}
