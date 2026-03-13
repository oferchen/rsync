//! Remote rsync invocation builder.
//!
//! This module constructs the command-line arguments for invoking rsync in
//! `--server` mode on a remote host via SSH. The invocation format mirrors
//! upstream rsync's `server_options()` function.
//!
//! Submodules:
//! - `builder` - `RemoteInvocationBuilder` for constructing server args.
//! - `transfer_role` - Transfer role detection and remote operand parsing.

mod builder;
#[cfg(test)]
mod tests;
mod transfer_role;

use std::ffi::OsString;

pub use builder::RemoteInvocationBuilder;
pub use transfer_role::{determine_transfer_role, operand_is_remote};

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
pub(super) struct RemoteOperandParsed {
    /// Full operand string (e.g., "user@host:/path").
    pub(super) operand: String,
    /// Host portion (e.g., "host" or "192.168.1.1" or "[::1]").
    pub(super) host: String,
    /// Optional user portion (e.g., "user").
    pub(super) user: Option<String>,
    /// Optional port (extracted from host if present).
    pub(super) port: Option<u16>,
}

/// Represents one or more remote operands in a transfer.
///
/// For push operations (local -> remote), there's always a single remote destination.
/// For pull operations (remote -> local), there can be multiple remote sources from
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
    /// Push transfer: local sources -> remote destination.
    ///
    /// The local process acts as generator/sender.
    Push {
        /// Local file paths to send.
        local_sources: Vec<String>,
        /// Remote destination operand (e.g., "user@host:/path").
        remote_dest: String,
    },

    /// Pull transfer: remote sources -> local destination.
    ///
    /// The local process acts as receiver.
    Pull {
        /// Remote source operand(s) (e.g., "user@host:/path").
        remote_sources: RemoteOperands,
        /// Local destination path.
        local_dest: String,
    },

    /// Proxy transfer: remote sources -> remote destination (via local).
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
