//! crates/daemon/src/daemon/connection_pool.rs
//!
//! Thread-safe connection pool using DashMap for concurrent access.
//!
//! This module provides connection tracking and rate limiting capabilities
//! that complement the file-based `ConnectionLimiter` for cross-process limits.
//! The pool uses DashMap for lock-free concurrent access, allowing multiple
//! threads to query and update connection state without blocking.

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Unique identifier for a connection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionId(u64);

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

/// Thread-safe connection pool for tracking active daemon connections.
///
/// Uses DashMap for lock-free concurrent access, enabling multiple threads
/// to query and update connection state without blocking the main accept loop.
///
/// # Example
///
/// ```ignore
/// use daemon::connection_pool::ConnectionPool;
/// use std::net::SocketAddr;
///
/// let pool = ConnectionPool::new();
///
/// // Register a new connection
/// let addr: SocketAddr = "192.168.1.100:12345".parse().unwrap();
/// let id = pool.register(addr);
///
/// // Set the module being accessed
/// pool.set_module(&id, "documents".to_string());
///
/// // Update byte counters
/// pool.add_bytes(&id, 1024, 512);
///
/// // Check rate limits by IP
/// let ip_addr = addr.ip();
/// if pool.connections_from_ip(&ip_addr) >= 10 {
///     println!("Rate limit exceeded for {}", ip_addr);
/// }
///
/// // Unregister when done
/// pool.unregister(&id);
/// ```
#[derive(Debug)]
pub struct ConnectionPool {
    /// Active connections indexed by ID.
    connections: DashMap<ConnectionId, ConnectionInfo>,
    /// Per-IP statistics for rate limiting.
    ip_stats: DashMap<IpAddr, IpStats>,
    /// Next connection ID to assign.
    next_id: AtomicU64,
}

impl ConnectionPool {
    /// Creates a new empty connection pool.
    #[must_use]
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            ip_stats: DashMap::new(),
            next_id: AtomicU64::new(1),
        }
    }

    /// Creates a new pool with the specified initial capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            connections: DashMap::with_capacity(capacity),
            ip_stats: DashMap::with_capacity(capacity),
            next_id: AtomicU64::new(1),
        }
    }

    /// Registers a new connection and returns its unique identifier.
    pub fn register(&self, peer_addr: SocketAddr) -> ConnectionId {
        let id = ConnectionId(self.next_id.fetch_add(1, Ordering::Relaxed));

        let info = ConnectionInfo {
            id,
            peer_addr,
            module: None,
            connected_at: Instant::now(),
            bytes_received: 0,
            bytes_sent: 0,
            active: true,
        };

        self.connections.insert(id, info);

        // Update per-IP statistics
        let ip = peer_addr.ip();
        self.ip_stats
            .entry(ip)
            .and_modify(|stats| {
                stats.active_connections = stats.active_connections.saturating_add(1);
                stats.total_connections = stats.total_connections.saturating_add(1);
                stats.last_seen = Instant::now();
            })
            .or_insert_with(|| {
                let now = Instant::now();
                IpStats {
                    active_connections: 1,
                    total_connections: 1,
                    bytes_received: 0,
                    bytes_sent: 0,
                    first_seen: now,
                    last_seen: now,
                }
            });

        id
    }

    /// Unregisters a connection from the pool.
    ///
    /// Returns the connection info if it existed.
    pub fn unregister(&self, id: &ConnectionId) -> Option<ConnectionInfo> {
        if let Some((_, info)) = self.connections.remove(id) {
            // Update per-IP statistics
            let ip = info.peer_addr.ip();
            if let Some(mut stats) = self.ip_stats.get_mut(&ip) {
                stats.active_connections = stats.active_connections.saturating_sub(1);
                stats.bytes_received = stats.bytes_received.saturating_add(info.bytes_received);
                stats.bytes_sent = stats.bytes_sent.saturating_add(info.bytes_sent);
            }
            Some(info)
        } else {
            None
        }
    }

    /// Returns the total number of registered connections.
    #[must_use]
    pub fn count(&self) -> usize {
        self.connections.len()
    }

    /// Returns the number of active connections.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.connections
            .iter()
            .filter(|entry| entry.value().active)
            .count()
    }

    /// Returns the number of active connections from a specific IP address.
    #[must_use]
    pub fn connections_from_ip(&self, ip: &IpAddr) -> u32 {
        self.ip_stats
            .get(ip)
            .map(|stats| stats.active_connections)
            .unwrap_or(0)
    }

    /// Checks if a new connection from the given IP would exceed the limit.
    #[must_use]
    pub fn would_exceed_ip_limit(&self, ip: &IpAddr, limit: NonZeroU32) -> bool {
        self.connections_from_ip(ip) >= limit.get()
    }

    /// Returns information about a specific connection.
    #[must_use]
    pub fn get(&self, id: &ConnectionId) -> Option<ConnectionInfo> {
        self.connections.get(id).map(|entry| entry.value().clone())
    }

    /// Returns statistics for a specific IP address.
    #[must_use]
    pub fn get_ip_stats(&self, ip: &IpAddr) -> Option<IpStats> {
        self.ip_stats.get(ip).map(|entry| entry.value().clone())
    }

    /// Sets the module being accessed by a connection.
    ///
    /// Returns `true` if the connection was found and updated.
    pub fn set_module(&self, id: &ConnectionId, module: String) -> bool {
        if let Some(mut entry) = self.connections.get_mut(id) {
            entry.module = Some(module);
            true
        } else {
            false
        }
    }

    /// Marks a connection as active or inactive.
    ///
    /// Returns `true` if the connection was found and updated.
    pub fn set_active(&self, id: &ConnectionId, active: bool) -> bool {
        if let Some(mut entry) = self.connections.get_mut(id) {
            entry.active = active;
            true
        } else {
            false
        }
    }

    /// Adds to the byte counters for a connection.
    ///
    /// Returns `true` if the connection was found and updated.
    pub fn add_bytes(&self, id: &ConnectionId, received: u64, sent: u64) -> bool {
        if let Some(mut entry) = self.connections.get_mut(id) {
            entry.bytes_received = entry.bytes_received.saturating_add(received);
            entry.bytes_sent = entry.bytes_sent.saturating_add(sent);
            true
        } else {
            false
        }
    }

    /// Returns a snapshot of all active connections.
    #[must_use]
    pub fn active_connections(&self) -> Vec<ConnectionInfo> {
        self.connections
            .iter()
            .filter(|entry| entry.value().active)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns a snapshot of all connections (including inactive).
    #[must_use]
    pub fn all_connections(&self) -> Vec<ConnectionInfo> {
        self.connections
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns connections accessing a specific module.
    #[must_use]
    pub fn connections_for_module(&self, module: &str) -> Vec<ConnectionInfo> {
        self.connections
            .iter()
            .filter(|entry| {
                entry
                    .value()
                    .module
                    .as_ref()
                    .is_some_and(|m| m == module)
            })
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns the count of connections accessing a specific module.
    #[must_use]
    pub fn module_connection_count(&self, module: &str) -> usize {
        self.connections
            .iter()
            .filter(|entry| {
                entry
                    .value()
                    .module
                    .as_ref()
                    .is_some_and(|m| m == module)
            })
            .count()
    }

    /// Returns connections from a specific IP address.
    #[must_use]
    pub fn connections_from_addr(&self, ip: &IpAddr) -> Vec<ConnectionInfo> {
        self.connections
            .iter()
            .filter(|entry| entry.value().peer_addr.ip() == *ip)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Removes inactive connections.
    ///
    /// Returns the number of connections removed.
    pub fn prune_inactive(&self) -> usize {
        let to_remove: Vec<ConnectionId> = self
            .connections
            .iter()
            .filter(|entry| !entry.value().active)
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len();
        for id in to_remove {
            self.unregister(&id);
        }
        count
    }

    /// Removes connections older than the specified duration.
    ///
    /// Returns the number of connections removed.
    pub fn prune_older_than(&self, max_age: Duration) -> usize {
        let to_remove: Vec<ConnectionId> = self
            .connections
            .iter()
            .filter(|entry| entry.value().duration() > max_age)
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len();
        for id in to_remove {
            self.unregister(&id);
        }
        count
    }

    /// Cleans up stale IP statistics for addresses with no active connections.
    ///
    /// Returns the number of IP entries removed.
    pub fn prune_stale_ip_stats(&self) -> usize {
        let to_remove: Vec<IpAddr> = self
            .ip_stats
            .iter()
            .filter(|entry| entry.value().active_connections == 0)
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len();
        for ip in to_remove {
            self.ip_stats.remove(&ip);
        }
        count
    }

    /// Returns the number of unique IP addresses with active connections.
    #[must_use]
    pub fn unique_ip_count(&self) -> usize {
        self.ip_stats
            .iter()
            .filter(|entry| entry.value().active_connections > 0)
            .count()
    }

    /// Returns aggregate statistics across all connections.
    #[must_use]
    pub fn aggregate_stats(&self) -> AggregateStats {
        let mut stats = AggregateStats::default();

        for entry in self.connections.iter() {
            let info = entry.value();
            stats.total_connections += 1;
            if info.active {
                stats.active_connections += 1;
            }
            stats.total_bytes_received += info.bytes_received;
            stats.total_bytes_sent += info.bytes_sent;
        }

        stats.unique_ips = self.unique_ip_count();
        stats
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn test_addr(ip_last: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, ip_last)), port)
    }

    #[test]
    fn register_creates_connection() {
        let pool = ConnectionPool::new();
        let id = pool.register(test_addr(100, 1234));

        assert_eq!(pool.count(), 1);
        let info = pool.get(&id).unwrap();
        assert_eq!(info.peer_addr, test_addr(100, 1234));
        assert!(info.active);
        assert!(info.module.is_none());
    }

    #[test]
    fn unregister_removes_connection() {
        let pool = ConnectionPool::new();
        let id = pool.register(test_addr(100, 1234));

        assert_eq!(pool.count(), 1);
        let removed = pool.unregister(&id);
        assert!(removed.is_some());
        assert_eq!(pool.count(), 0);
    }

    #[test]
    fn tracks_per_ip_connections() {
        let pool = ConnectionPool::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));

        // Register multiple connections from the same IP
        let id1 = pool.register(SocketAddr::new(ip, 1234));
        let id2 = pool.register(SocketAddr::new(ip, 1235));
        let id3 = pool.register(SocketAddr::new(ip, 1236));

        assert_eq!(pool.connections_from_ip(&ip), 3);

        // Unregister one
        pool.unregister(&id2);
        assert_eq!(pool.connections_from_ip(&ip), 2);

        // Unregister remaining
        pool.unregister(&id1);
        pool.unregister(&id3);
        assert_eq!(pool.connections_from_ip(&ip), 0);
    }

    #[test]
    fn would_exceed_ip_limit_works() {
        let pool = ConnectionPool::new();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let limit = NonZeroU32::new(3).unwrap();

        // Initially not exceeded
        assert!(!pool.would_exceed_ip_limit(&ip, limit));

        // Add connections
        pool.register(SocketAddr::new(ip, 1000));
        pool.register(SocketAddr::new(ip, 1001));
        assert!(!pool.would_exceed_ip_limit(&ip, limit));

        // At limit
        pool.register(SocketAddr::new(ip, 1002));
        assert!(pool.would_exceed_ip_limit(&ip, limit));
    }

    #[test]
    fn set_module_updates_connection() {
        let pool = ConnectionPool::new();
        let id = pool.register(test_addr(100, 1234));

        assert!(pool.set_module(&id, "documents".to_string()));
        let info = pool.get(&id).unwrap();
        assert_eq!(info.module, Some("documents".to_string()));
    }

    #[test]
    fn add_bytes_accumulates() {
        let pool = ConnectionPool::new();
        let id = pool.register(test_addr(100, 1234));

        pool.add_bytes(&id, 100, 50);
        pool.add_bytes(&id, 200, 100);

        let info = pool.get(&id).unwrap();
        assert_eq!(info.bytes_received, 300);
        assert_eq!(info.bytes_sent, 150);
    }

    #[test]
    fn connections_for_module_filters_correctly() {
        let pool = ConnectionPool::new();
        let id1 = pool.register(test_addr(100, 1234));
        let id2 = pool.register(test_addr(101, 1235));
        let id3 = pool.register(test_addr(102, 1236));

        pool.set_module(&id1, "docs".to_string());
        pool.set_module(&id2, "docs".to_string());
        pool.set_module(&id3, "photos".to_string());

        let docs_connections = pool.connections_for_module("docs");
        assert_eq!(docs_connections.len(), 2);
        assert_eq!(pool.module_connection_count("docs"), 2);
        assert_eq!(pool.module_connection_count("photos"), 1);
        assert_eq!(pool.module_connection_count("other"), 0);
    }

    #[test]
    fn prune_inactive_removes_only_inactive() {
        let pool = ConnectionPool::new();
        let id1 = pool.register(test_addr(100, 1234));
        let id2 = pool.register(test_addr(101, 1235));
        let id3 = pool.register(test_addr(102, 1236));

        pool.set_active(&id2, false);
        pool.set_active(&id3, false);

        let removed = pool.prune_inactive();
        assert_eq!(removed, 2);
        assert_eq!(pool.count(), 1);
        assert!(pool.get(&id1).is_some());
        assert!(pool.get(&id2).is_none());
        assert!(pool.get(&id3).is_none());
    }

    #[test]
    fn unique_ids_across_registrations() {
        let pool = ConnectionPool::new();
        let id1 = pool.register(test_addr(100, 1234));
        let id2 = pool.register(test_addr(101, 1235));
        let id3 = pool.register(test_addr(102, 1236));

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    #[test]
    fn aggregate_stats_computes_correctly() {
        let pool = ConnectionPool::new();
        let id1 = pool.register(test_addr(100, 1234));
        let id2 = pool.register(test_addr(100, 1235)); // Same IP
        let id3 = pool.register(test_addr(101, 1236)); // Different IP

        pool.add_bytes(&id1, 100, 50);
        pool.add_bytes(&id2, 200, 100);
        pool.add_bytes(&id3, 300, 150);
        pool.set_active(&id2, false);

        let stats = pool.aggregate_stats();
        assert_eq!(stats.total_connections, 3);
        assert_eq!(stats.active_connections, 2);
        assert_eq!(stats.unique_ips, 2); // Two unique IPs with active connections
        assert_eq!(stats.total_bytes_received, 600);
        assert_eq!(stats.total_bytes_sent, 300);
    }

    #[test]
    fn concurrent_access_is_safe() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(ConnectionPool::new());
        let mut handles = vec![];

        // Spawn multiple threads that register/unregister connections
        for i in 0..10 {
            let pool = Arc::clone(&pool);
            let handle = thread::spawn(move || {
                for j in 0..100 {
                    let port = (i * 1000 + j) as u16;
                    let id = pool.register(test_addr((i % 256) as u8, port));
                    pool.set_module(&id, format!("module_{i}"));
                    pool.add_bytes(&id, 100, 50);
                    pool.set_active(&id, false);
                    pool.unregister(&id);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All connections should be unregistered
        assert_eq!(pool.count(), 0);
    }

    #[test]
    fn ip_stats_accumulate_after_disconnect() {
        let pool = ConnectionPool::new();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));

        // First connection
        let id1 = pool.register(SocketAddr::new(ip, 1000));
        pool.add_bytes(&id1, 100, 50);
        pool.unregister(&id1);

        // Second connection
        let id2 = pool.register(SocketAddr::new(ip, 1001));
        pool.add_bytes(&id2, 200, 100);
        pool.unregister(&id2);

        // Check accumulated stats
        let stats = pool.get_ip_stats(&ip).unwrap();
        assert_eq!(stats.active_connections, 0);
        assert_eq!(stats.total_connections, 2);
        assert_eq!(stats.bytes_received, 300);
        assert_eq!(stats.bytes_sent, 150);
    }
}
