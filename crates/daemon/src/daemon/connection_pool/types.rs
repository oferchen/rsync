//! Data types for connection tracking and IP-level statistics.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Unique identifier for a connection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionId(pub(super) u64);

impl ConnectionId {
    /// Returns the numeric value of this connection ID.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Metadata about an active connection.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ConnectionInfo {
    /// Unique connection identifier.
    pub id: ConnectionId,
    /// Remote peer address.
    pub peer_addr: SocketAddr,
    /// Module being accessed (if determined).
    pub module: Option<String>,
    /// Time when connection was established.
    pub connected_at: Instant,
    /// Total bytes received from this connection.
    pub bytes_received: u64,
    /// Total bytes sent to this connection.
    pub bytes_sent: u64,
    /// Whether the connection is currently active.
    pub active: bool,
}

impl ConnectionInfo {
    /// Returns how long this connection has been open.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.connected_at.elapsed()
    }
}

/// Per-IP address statistics for rate limiting.
#[derive(Clone, Debug)]
pub struct IpStats {
    /// Number of active connections from this IP.
    pub active_connections: u32,
    /// Total connections ever made from this IP.
    pub total_connections: u64,
    /// Total bytes received from this IP.
    pub bytes_received: u64,
    /// Total bytes sent to this IP.
    pub bytes_sent: u64,
    /// Time of first connection from this IP.
    pub first_seen: Instant,
    /// Time of last connection from this IP.
    pub last_seen: Instant,
}

impl Default for IpStats {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            active_connections: 0,
            total_connections: 0,
            bytes_received: 0,
            bytes_sent: 0,
            first_seen: now,
            last_seen: now,
        }
    }
}

/// Aggregate statistics across all connections.
#[derive(Clone, Debug, Default)]
pub struct AggregateStats {
    /// Total number of registered connections.
    pub total_connections: usize,
    /// Number of currently active connections.
    pub active_connections: usize,
    /// Number of unique IP addresses with active connections.
    pub unique_ips: usize,
    /// Total bytes received across all connections.
    pub total_bytes_received: u64,
    /// Total bytes sent across all connections.
    pub total_bytes_sent: u64,
}
