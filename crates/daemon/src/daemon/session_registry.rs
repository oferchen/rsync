//! crates/daemon/src/daemon/session_registry.rs
//!
//! Concurrent session tracking using DashMap.
//!
//! This module provides a thread-safe registry for tracking active daemon
//! sessions. It enables concurrent queries about session state without
//! blocking the main accept loop.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Unique identifier for a session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId(u64);

impl SessionId {
    /// Returns the numeric value of this session ID.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Current state of a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    /// Session is negotiating the protocol handshake.
    Handshaking,
    /// Session is authenticating with the daemon.
    Authenticating,
    /// Session is listing available modules.
    Listing,
    /// Session is actively transferring data.
    Transferring,
    /// Session completed successfully.
    Completed,
    /// Session failed with an error.
    Failed,
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handshaking => write!(f, "handshaking"),
            Self::Authenticating => write!(f, "authenticating"),
            Self::Listing => write!(f, "listing"),
            Self::Transferring => write!(f, "transferring"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Metadata about an active session.
#[derive(Clone, Debug)]
pub struct SessionInfo {
    /// Unique session identifier.
    pub id: SessionId,
    /// Remote peer address.
    pub peer_addr: SocketAddr,
    /// Resolved hostname (if reverse lookup enabled).
    pub peer_hostname: Option<String>,
    /// Module being accessed (if determined).
    pub module: Option<String>,
    /// Current session state.
    pub state: SessionState,
    /// Time when session started.
    pub started_at: Instant,
    /// Bytes received from client.
    pub bytes_received: u64,
    /// Bytes sent to client.
    pub bytes_sent: u64,
}

impl SessionInfo {
    /// Returns how long this session has been active.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Returns whether the session is still active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            SessionState::Handshaking
                | SessionState::Authenticating
                | SessionState::Listing
                | SessionState::Transferring
        )
    }
}

/// Thread-safe registry for tracking active daemon sessions.
///
/// Uses DashMap for lock-free concurrent access, allowing multiple
/// threads to query session state without blocking the main accept loop.
///
/// # Example
///
/// ```ignore
/// use daemon::session_registry::SessionRegistry;
/// use std::net::SocketAddr;
///
/// let registry = SessionRegistry::new();
///
/// // Register a new session
/// let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
/// let id = registry.register(addr, None);
///
/// // Update session state
/// registry.set_module(&id, "documents".to_string());
/// registry.set_state(&id, SessionState::Transferring);
///
/// // Query active sessions
/// let active = registry.active_count();
/// println!("Active sessions: {}", active);
///
/// // Remove when done
/// registry.unregister(&id);
/// ```
#[derive(Debug)]
pub struct SessionRegistry {
    sessions: DashMap<SessionId, SessionInfo>,
    next_id: AtomicU64,
}

impl SessionRegistry {
    /// Creates a new empty session registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            next_id: AtomicU64::new(1),
        }
    }

    /// Creates a new registry with the specified initial capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            sessions: DashMap::with_capacity(capacity),
            next_id: AtomicU64::new(1),
        }
    }

    /// Registers a new session and returns its unique identifier.
    pub fn register(&self, peer_addr: SocketAddr, peer_hostname: Option<String>) -> SessionId {
        let id = SessionId(self.next_id.fetch_add(1, Ordering::Relaxed));

        let info = SessionInfo {
            id,
            peer_addr,
            peer_hostname,
            module: None,
            state: SessionState::Handshaking,
            started_at: Instant::now(),
            bytes_received: 0,
            bytes_sent: 0,
        };

        self.sessions.insert(id, info);
        id
    }

    /// Removes a session from the registry.
    ///
    /// Returns the session info if it existed.
    pub fn unregister(&self, id: &SessionId) -> Option<SessionInfo> {
        self.sessions.remove(id).map(|(_, info)| info)
    }

    /// Returns the number of currently registered sessions.
    #[must_use]
    pub fn count(&self) -> usize {
        self.sessions.len()
    }

    /// Returns the number of active (non-terminal) sessions.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|entry| entry.value().is_active())
            .count()
    }

    /// Returns information about a specific session.
    #[must_use]
    pub fn get(&self, id: &SessionId) -> Option<SessionInfo> {
        self.sessions.get(id).map(|entry| entry.value().clone())
    }

    /// Updates the state of a session.
    ///
    /// Returns `true` if the session was found and updated.
    pub fn set_state(&self, id: &SessionId, state: SessionState) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(id) {
            entry.state = state;
            true
        } else {
            false
        }
    }

    /// Sets the module being accessed by a session.
    ///
    /// Returns `true` if the session was found and updated.
    pub fn set_module(&self, id: &SessionId, module: String) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(id) {
            entry.module = Some(module);
            true
        } else {
            false
        }
    }

    /// Updates the byte counters for a session.
    ///
    /// Returns `true` if the session was found and updated.
    pub fn update_bytes(&self, id: &SessionId, received: u64, sent: u64) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(id) {
            entry.bytes_received = received;
            entry.bytes_sent = sent;
            true
        } else {
            false
        }
    }

    /// Adds to the byte counters for a session.
    ///
    /// Returns `true` if the session was found and updated.
    pub fn add_bytes(&self, id: &SessionId, received: u64, sent: u64) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(id) {
            entry.bytes_received = entry.bytes_received.saturating_add(received);
            entry.bytes_sent = entry.bytes_sent.saturating_add(sent);
            true
        } else {
            false
        }
    }

    /// Returns a snapshot of all active sessions.
    #[must_use]
    pub fn active_sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .filter(|entry| entry.value().is_active())
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns a snapshot of all sessions (including completed/failed).
    #[must_use]
    pub fn all_sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns sessions accessing a specific module.
    #[must_use]
    pub fn sessions_for_module(&self, module: &str) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .filter(|entry| entry.value().module.as_ref().is_some_and(|m| m == module))
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns the count of sessions accessing a specific module.
    #[must_use]
    pub fn module_session_count(&self, module: &str) -> usize {
        self.sessions
            .iter()
            .filter(|entry| entry.value().module.as_ref().is_some_and(|m| m == module))
            .count()
    }

    /// Removes all completed or failed sessions.
    ///
    /// Returns the number of sessions removed.
    pub fn prune_inactive(&self) -> usize {
        let to_remove: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|entry| !entry.value().is_active())
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len();
        for id in to_remove {
            self.sessions.remove(&id);
        }
        count
    }

    /// Removes sessions older than the specified duration.
    ///
    /// Returns the number of sessions removed.
    pub fn prune_older_than(&self, max_age: Duration) -> usize {
        let to_remove: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|entry| entry.value().duration() > max_age)
            .map(|entry| *entry.key())
            .collect();

        let count = to_remove.len();
        for id in to_remove {
            self.sessions.remove(&id);
        }
        count
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(port: u16) -> SocketAddr {
        use std::net::{IpAddr, Ipv4Addr};
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
    }

    #[test]
    fn register_creates_session() {
        let registry = SessionRegistry::new();
        let id = registry.register(test_addr(1234), None);

        assert_eq!(registry.count(), 1);
        let info = registry.get(&id).unwrap();
        assert_eq!(info.peer_addr, test_addr(1234));
        assert_eq!(info.state, SessionState::Handshaking);
        assert!(info.module.is_none());
    }

    #[test]
    fn unregister_removes_session() {
        let registry = SessionRegistry::new();
        let id = registry.register(test_addr(1234), None);

        assert_eq!(registry.count(), 1);
        let removed = registry.unregister(&id);
        assert!(removed.is_some());
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn set_state_updates_session() {
        let registry = SessionRegistry::new();
        let id = registry.register(test_addr(1234), None);

        assert!(registry.set_state(&id, SessionState::Transferring));
        let info = registry.get(&id).unwrap();
        assert_eq!(info.state, SessionState::Transferring);
    }

    #[test]
    fn set_module_updates_session() {
        let registry = SessionRegistry::new();
        let id = registry.register(test_addr(1234), None);

        assert!(registry.set_module(&id, "documents".to_string()));
        let info = registry.get(&id).unwrap();
        assert_eq!(info.module, Some("documents".to_string()));
    }

    #[test]
    fn active_count_excludes_completed() {
        let registry = SessionRegistry::new();
        let id1 = registry.register(test_addr(1234), None);
        let id2 = registry.register(test_addr(1235), None);
        let id3 = registry.register(test_addr(1236), None);

        registry.set_state(&id2, SessionState::Completed);
        registry.set_state(&id3, SessionState::Failed);

        assert_eq!(registry.count(), 3);
        assert_eq!(registry.active_count(), 1);

        // id1 should be the only active session
        let active = registry.active_sessions();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, id1);
    }

    #[test]
    fn sessions_for_module_filters_correctly() {
        let registry = SessionRegistry::new();
        let id1 = registry.register(test_addr(1234), None);
        let id2 = registry.register(test_addr(1235), None);
        let id3 = registry.register(test_addr(1236), None);

        registry.set_module(&id1, "docs".to_string());
        registry.set_module(&id2, "docs".to_string());
        registry.set_module(&id3, "photos".to_string());

        let docs_sessions = registry.sessions_for_module("docs");
        assert_eq!(docs_sessions.len(), 2);
        assert_eq!(registry.module_session_count("docs"), 2);
        assert_eq!(registry.module_session_count("photos"), 1);
        assert_eq!(registry.module_session_count("other"), 0);
    }

    #[test]
    fn add_bytes_accumulates() {
        let registry = SessionRegistry::new();
        let id = registry.register(test_addr(1234), None);

        registry.add_bytes(&id, 100, 50);
        registry.add_bytes(&id, 200, 100);

        let info = registry.get(&id).unwrap();
        assert_eq!(info.bytes_received, 300);
        assert_eq!(info.bytes_sent, 150);
    }

    #[test]
    fn prune_inactive_removes_terminal_sessions() {
        let registry = SessionRegistry::new();
        let id1 = registry.register(test_addr(1234), None);
        let id2 = registry.register(test_addr(1235), None);
        let id3 = registry.register(test_addr(1236), None);

        registry.set_state(&id2, SessionState::Completed);
        registry.set_state(&id3, SessionState::Failed);

        let removed = registry.prune_inactive();
        assert_eq!(removed, 2);
        assert_eq!(registry.count(), 1);
        assert!(registry.get(&id1).is_some());
        assert!(registry.get(&id2).is_none());
        assert!(registry.get(&id3).is_none());
    }

    #[test]
    fn unique_ids_across_registrations() {
        let registry = SessionRegistry::new();
        let id1 = registry.register(test_addr(1234), None);
        let id2 = registry.register(test_addr(1235), None);
        let id3 = registry.register(test_addr(1236), None);

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    #[test]
    fn concurrent_access_is_safe() {
        use std::sync::Arc;
        use std::thread;

        let registry = Arc::new(SessionRegistry::new());
        let mut handles = vec![];

        // Spawn multiple threads that register/unregister sessions
        for i in 0..10 {
            let registry = Arc::clone(&registry);
            let handle = thread::spawn(move || {
                for j in 0..100 {
                    let port = (i * 1000 + j) as u16;
                    let id = registry.register(test_addr(port), None);
                    registry.set_state(&id, SessionState::Transferring);
                    registry.add_bytes(&id, 100, 50);
                    registry.set_state(&id, SessionState::Completed);
                    registry.unregister(&id);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All sessions should be unregistered
        assert_eq!(registry.count(), 0);
    }
}
