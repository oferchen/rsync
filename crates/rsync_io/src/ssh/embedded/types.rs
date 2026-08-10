//! Configuration enums for the embedded SSH transport.
//!
//! These types mirror OpenSSH client options and control host key
//! verification policy and IP version preference for DNS resolution.

/// Host key verification policy.
///
/// Controls behavior when the remote server's host key is not recognized.
/// Mirrors the SSH `StrictHostKeyChecking` option semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictHostKeyChecking {
    /// Reject connections to hosts with unknown or mismatched keys.
    ///
    /// Safest option - requires the host key to already exist in the
    /// known hosts file. New hosts must be added manually.
    Yes,
    /// Accept unknown host keys without prompting and persist them.
    ///
    /// Insecure - vulnerable to MITM on first connect. Changed keys
    /// are still rejected. Useful for automated/batch transfers.
    No,
    /// Prompt the user interactively when encountering an unknown host key.
    ///
    /// Default mode, matching OpenSSH behavior. Falls back to rejection
    /// when no TTY is available (e.g., backgrounded or piped).
    Ask,
}

impl Default for StrictHostKeyChecking {
    fn default() -> Self {
        Self::Ask
    }
}

/// IP version preference for DNS resolution.
///
/// Controls whether the SSH transport resolves hostnames to IPv4 or IPv6
/// addresses. Mirrors the SSH `-4`/`-6` flag behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpPreference {
    /// Let the system choose based on available addresses.
    Auto,
    /// Prefer IPv6 addresses when both are available. Mirrors `ssh -6` with
    /// fallback to IPv4.
    PreferV6,
    /// Only resolve and connect to IPv4 addresses. Mirrors `ssh -4`.
    ForceV4,
    /// Only resolve and connect to IPv6 addresses. Mirrors `ssh -6`.
    ForceV6,
}

impl Default for IpPreference {
    fn default() -> Self {
        Self::Auto
    }
}
