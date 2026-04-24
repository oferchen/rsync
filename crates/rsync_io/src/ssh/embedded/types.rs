//! Configuration enums for the embedded SSH transport.

/// Host key verification policy.
///
/// Controls behavior when the remote server's host key is not recognized.
/// Mirrors the SSH `StrictHostKeyChecking` option semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictHostKeyChecking {
    /// Reject connections to hosts with unknown or mismatched keys.
    Yes,
    /// Accept any host key without verification (insecure).
    No,
    /// Prompt the user when encountering an unknown host key.
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
    /// Let the system choose (default: prefer IPv4 if both available).
    Auto,
    /// Prefer IPv6 addresses when available.
    PreferV6,
    /// Only use IPv4 addresses.
    ForceV4,
    /// Only use IPv6 addresses.
    ForceV6,
}

impl Default for IpPreference {
    fn default() -> Self {
        Self::Auto
    }
}
