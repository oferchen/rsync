//! Thread-safe connection pool with per-IP rate limiting.
//!
//! Uses `DashMap` for lock-free concurrent access, allowing multiple threads
//! to query and update connection state without blocking.

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use super::types::{AggregateStats, ConnectionId, ConnectionInfo, IpStats};

/// Thread-safe connection pool for tracking active daemon connections.
///
/// Uses `DashMap` for lock-free concurrent access, enabling multiple threads
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

#[allow(dead_code)]
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
    pub fn get(&self, id: &ConnectionId) -> Option<ConnectionInfo> {
        self.connections.get(id).map(|entry| entry.value().clone())
    }

    /// Returns statistics for a specific IP address.
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
            .filter(|entry| entry.value().module.as_ref().is_some_and(|m| m == module))
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns the count of connections accessing a specific module.
    #[must_use]
    pub fn module_connection_count(&self, module: &str) -> usize {
        self.connections
            .iter()
            .filter(|entry| entry.value().module.as_ref().is_some_and(|m| m == module))
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
