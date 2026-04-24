//! Error types for the embedded SSH transport.
//!
//! Covers every failure mode in the SSH connection lifecycle: URL parsing,
//! TCP connect, host key verification, authentication, key loading, and
//! I/O during data transfer.

/// Errors from the embedded SSH transport layer.
///
/// Each variant maps to a distinct failure mode in the SSH connection
/// lifecycle - from URL parsing through authentication to data transfer.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// SSH protocol or connection error from the russh library.
    #[error("SSH connection error: {0}")]
    Connect(#[from] russh::Error),

    /// All authentication methods were exhausted without success.
    #[error("authentication failed (tried: {tried})")]
    AuthenticationFailed {
        /// Comma-separated list of methods attempted.
        tried: String,
    },

    /// Server's host key does not match the known key.
    #[error("host key mismatch for {host}")]
    HostKeyMismatch {
        /// Hostname or IP that presented the mismatched key.
        host: String,
    },

    /// Server's host key is not in the known hosts file and strict checking is enabled.
    #[error("unknown host: {host}")]
    UnknownHost {
        /// Hostname or IP with no known key.
        host: String,
    },

    /// Failed to parse an ssh:// URL.
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to load an SSH private key.
    #[error("key load error: {0}")]
    KeyLoad(String),

    /// Connection or operation timed out.
    #[error("timeout after {secs}s")]
    Timeout {
        /// Number of seconds before timeout.
        secs: u64,
    },

    /// The URL was syntactically valid but semantically invalid for SSH.
    #[error("invalid SSH URL: {reason}")]
    InvalidUrl {
        /// Description of why the URL is invalid.
        reason: String,
    },
}
