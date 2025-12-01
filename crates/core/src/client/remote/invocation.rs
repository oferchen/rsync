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
        if colon_index == 1 && text.chars().next().map_or(false, |c| c.is_ascii_alphabetic()) {
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
    pub fn new(config: &'a ClientConfig, role: RemoteRole) -> Self {
        Self { config, role }
    }

    /// Builds the complete invocation argument vector.
    ///
    /// The first element is always "rsync", followed by "--server", optional
    /// role flags, the compact flag string, ".", and the remote path.
    pub fn build(self, remote_path: &str) -> Vec<OsString> {
        let mut args = Vec::new();

        args.push(OsString::from("rsync"));
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

        // Remote path comes last
        args.push(OsString::from(remote_path));

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
/// - `Vec<String>` - Local paths (for sender role)
/// - `String` - Remote operand string
///
/// # Errors
///
/// Returns error if:
/// - Both source and destination are remote (not yet supported)
/// - Neither source nor destination is remote (should use local copy)
/// - Multiple sources with different remote/local mix
pub fn determine_transfer_role(
    sources: &[OsString],
    destination: &OsString,
) -> Result<(RemoteRole, Vec<String>, String), ClientError> {
    let dest_is_remote = operand_is_remote(destination);

    // Check if any sources are remote
    let remote_sources: Vec<_> = sources
        .iter()
        .filter(|s| operand_is_remote(s))
        .collect();

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
            Err(invalid_argument_error(
                "no remote operand found",
                1,
            ))
        }
        (true, false) => {
            // Pull: remote source → local destination
            if !all_sources_remote {
                return Err(invalid_argument_error(
                    "mixing remote and local sources is not supported",
                    1,
                ));
            }

            if sources.len() > 1 {
                return Err(invalid_argument_error(
                    "multiple remote sources are not yet supported",
                    1,
                ));
            }

            let remote_operand = sources[0].to_string_lossy().to_string();
            let local_path = destination.to_string_lossy().to_string();

            Ok((RemoteRole::Receiver, vec![local_path], remote_operand))
        }
        (false, true) => {
            // Push: local source(s) → remote destination
            let local_paths: Vec<String> = sources
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();

            let remote_operand = destination.to_string_lossy().to_string();

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
        assert_eq!(result.2, "user@host:/remote.txt");
    }

    #[test]
    fn detects_pull_when_source_remote() {
        let sources = vec![OsString::from("user@host:/remote.txt")];
        let destination = OsString::from("local.txt");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.0, RemoteRole::Receiver);
        assert_eq!(result.1, vec!["local.txt"]);
        assert_eq!(result.2, "user@host:/remote.txt");
    }

    #[test]
    fn detects_push_with_multiple_sources() {
        let sources = vec![
            OsString::from("file1.txt"),
            OsString::from("file2.txt"),
        ];
        let destination = OsString::from("host:/dest/");

        let result = determine_transfer_role(&sources, &destination).unwrap();

        assert_eq!(result.0, RemoteRole::Sender);
        assert_eq!(result.1, vec!["file1.txt", "file2.txt"]);
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
    fn rejects_multiple_remote_sources() {
        let sources = vec![
            OsString::from("host1:/file1"),
            OsString::from("host2:/file2"),
        ];
        let destination = OsString::from("dest/");

        let result = determine_transfer_role(&sources, &destination);
        assert!(result.is_err());
    }
}
