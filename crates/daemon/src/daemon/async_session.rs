//! crates/daemon/src/daemon/async_session.rs
//!
//! Async session handling for the rsync daemon.
//!
//! This module provides tokio-based async alternatives to synchronous session
//! handling. It is only available when the `async` feature is enabled.
//!
//! # Features
//!
//! - Async TCP listener with configurable connection limits
//! - Async session handling with timeout support
//! - Integration with SessionRegistry for concurrent session tracking
//! - Graceful shutdown support via cancellation tokens
//!
//! # Example
//!
//! ```ignore
//! use daemon::async_session::{AsyncDaemonListener, ListenerConfig};
//!
//! let config = ListenerConfig::new()
//!     .bind_address("0.0.0.0:873".parse().unwrap())
//!     .max_connections(100);
//!
//! let listener = AsyncDaemonListener::bind(config).await?;
//! listener.serve().await?;
//! ```

use std::io;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Semaphore};
use tokio::time::timeout;

#[cfg(feature = "concurrent-sessions")]
use super::session_registry::{SessionId, SessionRegistry, SessionState};

#[cfg(feature = "concurrent-sessions")]
use super::connection_pool::{ConnectionId, ConnectionPool};

/// Default maximum number of concurrent connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 200;

/// Default connection timeout in seconds.
pub const DEFAULT_CONNECTION_TIMEOUT: u64 = 60;

/// Default read timeout for session I/O in seconds.
pub const DEFAULT_READ_TIMEOUT: u64 = 30;

/// Configuration for the async daemon listener.
#[derive(Debug, Clone)]
pub struct ListenerConfig {
    /// Address to bind to.
    pub bind_address: SocketAddr,
    /// Maximum number of concurrent connections.
    pub max_connections: usize,
    /// Connection acceptance timeout.
    pub connection_timeout: Duration,
    /// Read timeout for session I/O.
    pub read_timeout: Duration,
    /// Whether to enable TCP keepalive.
    pub tcp_keepalive: bool,
    /// TCP keepalive interval in seconds.
    pub keepalive_interval: u64,
}

impl Default for ListenerConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenerConfig {
    /// Creates a new listener configuration with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bind_address: "0.0.0.0:873".parse().unwrap(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            connection_timeout: Duration::from_secs(DEFAULT_CONNECTION_TIMEOUT),
            read_timeout: Duration::from_secs(DEFAULT_READ_TIMEOUT),
            tcp_keepalive: true,
            keepalive_interval: 60,
        }
    }

    /// Sets the bind address.
    #[must_use]
    pub fn bind_address(mut self, addr: SocketAddr) -> Self {
        self.bind_address = addr;
        self
    }

    /// Sets the maximum number of concurrent connections.
    #[must_use]
    pub fn max_connections(mut self, max: usize) -> Self {
        self.max_connections = max.max(1);
        self
    }

    /// Sets the connection timeout.
    #[must_use]
    pub fn connection_timeout(mut self, timeout: Duration) -> Self {
        self.connection_timeout = timeout;
        self
    }

    /// Sets the read timeout for session I/O.
    #[must_use]
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// Enables or disables TCP keepalive.
    #[must_use]
    pub fn tcp_keepalive(mut self, enable: bool) -> Self {
        self.tcp_keepalive = enable;
        self
    }
}

/// Error type for async daemon operations.
#[derive(Debug)]
pub enum AsyncDaemonError {
    /// I/O error during daemon operation.
    Io(io::Error),

    /// Connection limit reached.
    ConnectionLimitReached(usize),

    /// Session timeout.
    Timeout(Duration),

    /// Shutdown signal received.
    Shutdown,

    /// Protocol error.
    Protocol(String),
}

impl std::fmt::Display for AsyncDaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::ConnectionLimitReached(max) => {
                write!(f, "Maximum connections ({max}) reached")
            }
            Self::Timeout(d) => write!(f, "Session timed out after {d:?}"),
            Self::Shutdown => write!(f, "Daemon shutdown requested"),
            Self::Protocol(msg) => write!(f, "Protocol error: {msg}"),
        }
    }
}

impl std::error::Error for AsyncDaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for AsyncDaemonError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Async TCP listener for the rsync daemon.
pub struct AsyncDaemonListener {
    listener: TcpListener,
    config: ListenerConfig,
    connection_semaphore: Arc<Semaphore>,
    shutdown_tx: broadcast::Sender<()>,
    #[cfg(feature = "concurrent-sessions")]
    session_registry: Arc<SessionRegistry>,
    #[cfg(feature = "concurrent-sessions")]
    connection_pool: Arc<ConnectionPool>,
}

impl AsyncDaemonListener {
    /// Creates a new async daemon listener bound to the configured address.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener cannot bind to the address.
    pub async fn bind(config: ListenerConfig) -> Result<Self, AsyncDaemonError> {
        let listener = TcpListener::bind(config.bind_address).await?;
        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Self {
            listener,
            connection_semaphore: Arc::new(Semaphore::new(config.max_connections)),
            config,
            shutdown_tx,
            #[cfg(feature = "concurrent-sessions")]
            session_registry: Arc::new(SessionRegistry::new()),
            #[cfg(feature = "concurrent-sessions")]
            connection_pool: Arc::new(ConnectionPool::new()),
        })
    }

    /// Returns the local address the listener is bound to.
    ///
    /// # Errors
    ///
    /// Returns an error if the local address cannot be determined.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Returns a shutdown signal sender for graceful shutdown.
    #[must_use]
    pub fn shutdown_signal(&self) -> broadcast::Sender<()> {
        self.shutdown_tx.clone()
    }

    /// Returns the session registry (only with concurrent-sessions feature).
    #[cfg(feature = "concurrent-sessions")]
    #[must_use]
    pub fn session_registry(&self) -> &Arc<SessionRegistry> {
        &self.session_registry
    }

    /// Returns the connection pool (only with concurrent-sessions feature).
    #[cfg(feature = "concurrent-sessions")]
    #[must_use]
    pub fn connection_pool(&self) -> &Arc<ConnectionPool> {
        &self.connection_pool
    }

    /// Serves connections until shutdown is requested.
    ///
    /// This method spawns a new task for each accepted connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener encounters an unrecoverable error.
    pub async fn serve(self) -> Result<(), AsyncDaemonError> {
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            tokio::select! {
                result = self.listener.accept() => {
                    let (stream, peer_addr) = result?;

                    // Try to acquire a connection permit
                    let permit = match self.connection_semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            // Connection limit reached - close immediately
                            drop(stream);
                            continue;
                        }
                    };

                    // Register the connection
                    #[cfg(feature = "concurrent-sessions")]
                    let session_id = self.session_registry.register(peer_addr, None);
                    #[cfg(feature = "concurrent-sessions")]
                    let conn_id = self.connection_pool.register(peer_addr);

                    let config = self.config.clone();
                    #[cfg(feature = "concurrent-sessions")]
                    let registry = self.session_registry.clone();
                    #[cfg(feature = "concurrent-sessions")]
                    let pool = self.connection_pool.clone();

                    // Spawn handler task
                    tokio::spawn(async move {
                        let result = handle_async_session(
                            stream,
                            peer_addr,
                            &config,
                            #[cfg(feature = "concurrent-sessions")]
                            &session_id,
                            #[cfg(feature = "concurrent-sessions")]
                            &conn_id,
                            #[cfg(feature = "concurrent-sessions")]
                            &registry,
                            #[cfg(feature = "concurrent-sessions")]
                            &pool,
                        )
                        .await;

                        // Unregister on completion
                        #[cfg(feature = "concurrent-sessions")]
                        {
                            registry.unregister(&session_id);
                            pool.unregister(&conn_id);
                        }

                        // Release permit
                        drop(permit);

                        if let Err(e) = result {
                            // Log error (in a real implementation)
                            let _ = e;
                        }
                    });
                }

                _ = shutdown_rx.recv() => {
                    return Ok(());
                }
            }
        }
    }

    /// Accepts a single connection and handles it.
    ///
    /// # Errors
    ///
    /// Returns an error if accepting or handling the connection fails.
    pub async fn accept_one(&self) -> Result<AsyncSession, AsyncDaemonError> {
        let _permit = self
            .connection_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AsyncDaemonError::Shutdown)?;

        let (stream, peer_addr) = self.listener.accept().await?;

        #[cfg(feature = "concurrent-sessions")]
        let session_id = self.session_registry.register(peer_addr, None);
        #[cfg(feature = "concurrent-sessions")]
        let conn_id = self.connection_pool.register(peer_addr);

        Ok(AsyncSession {
            stream: Some(stream),
            peer_addr,
            config: self.config.clone(),
            #[cfg(feature = "concurrent-sessions")]
            session_id,
            #[cfg(feature = "concurrent-sessions")]
            conn_id,
            #[cfg(feature = "concurrent-sessions")]
            registry: self.session_registry.clone(),
            #[cfg(feature = "concurrent-sessions")]
            pool: self.connection_pool.clone(),
        })
    }
}

/// An async daemon session.
pub struct AsyncSession {
    stream: Option<TcpStream>,
    peer_addr: SocketAddr,
    config: ListenerConfig,
    #[cfg(feature = "concurrent-sessions")]
    session_id: SessionId,
    #[cfg(feature = "concurrent-sessions")]
    conn_id: ConnectionId,
    #[cfg(feature = "concurrent-sessions")]
    registry: Arc<SessionRegistry>,
    #[cfg(feature = "concurrent-sessions")]
    pool: Arc<ConnectionPool>,
}

impl AsyncSession {
    /// Returns the peer address.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Returns the session ID (only with concurrent-sessions feature).
    #[cfg(feature = "concurrent-sessions")]
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the connection ID (only with concurrent-sessions feature).
    #[cfg(feature = "concurrent-sessions")]
    #[must_use]
    pub fn conn_id(&self) -> &ConnectionId {
        &self.conn_id
    }

    /// Handles the session using the legacy rsync daemon protocol.
    ///
    /// # Errors
    ///
    /// Returns an error if the session handling fails.
    pub async fn handle(mut self) -> Result<SessionResult, AsyncDaemonError> {
        let stream = self
            .stream
            .take()
            .ok_or_else(|| AsyncDaemonError::Protocol("Session already handled".to_string()))?;

        handle_async_session(
            stream,
            self.peer_addr,
            &self.config,
            #[cfg(feature = "concurrent-sessions")]
            &self.session_id,
            #[cfg(feature = "concurrent-sessions")]
            &self.conn_id,
            #[cfg(feature = "concurrent-sessions")]
            &self.registry,
            #[cfg(feature = "concurrent-sessions")]
            &self.pool,
        )
        .await
    }
}

impl Drop for AsyncSession {
    fn drop(&mut self) {
        #[cfg(feature = "concurrent-sessions")]
        {
            self.registry.unregister(&self.session_id);
            self.pool.unregister(&self.conn_id);
        }
    }
}

/// Result of a session handling operation.
#[derive(Debug, Clone)]
pub struct SessionResult {
    /// Peer address of the session.
    pub peer_addr: SocketAddr,
    /// Bytes received during the session.
    pub bytes_received: u64,
    /// Bytes sent during the session.
    pub bytes_sent: u64,
    /// Module that was accessed (if any).
    pub module: Option<String>,
    /// Whether the session completed successfully.
    pub success: bool,
}

/// Handles an async session.
#[allow(clippy::too_many_arguments)]
async fn handle_async_session(
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: &ListenerConfig,
    #[cfg(feature = "concurrent-sessions")] session_id: &SessionId,
    #[cfg(feature = "concurrent-sessions")] conn_id: &ConnectionId,
    #[cfg(feature = "concurrent-sessions")] registry: &SessionRegistry,
    #[cfg(feature = "concurrent-sessions")] pool: &ConnectionPool,
) -> Result<SessionResult, AsyncDaemonError> {
    #[cfg(feature = "concurrent-sessions")]
    registry.set_state(session_id, SessionState::Handshaking);

    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);

    let mut bytes_received: u64 = 0;
    let mut bytes_sent: u64 = 0;

    // Send greeting
    let greeting = format!("@RSYNCD: {}.0\n", 32); // Protocol version 32
    writer.write_all(greeting.as_bytes()).await?;
    writer.flush().await?;
    bytes_sent += greeting.len() as u64;

    // Read client version with timeout
    let mut line_buf = String::new();
    let read_result = timeout(config.read_timeout, reader.read_line(&mut line_buf)).await;

    let version_line = match read_result {
        Ok(Ok(n)) if n > 0 => {
            bytes_received += n as u64;
            line_buf.trim().to_string()
        }
        Ok(Ok(_)) => {
            // EOF
            return Ok(SessionResult {
                peer_addr,
                bytes_received,
                bytes_sent,
                module: None,
                success: false,
            });
        }
        Ok(Err(e)) => return Err(AsyncDaemonError::Io(e)),
        Err(_) => return Err(AsyncDaemonError::Timeout(config.read_timeout)),
    };

    // Parse client version
    let _client_version = if version_line.starts_with("@RSYNCD:") {
        version_line
            .strip_prefix("@RSYNCD:")
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(28.0) as u8
    } else {
        28 // Default to protocol 28
    };

    #[cfg(feature = "concurrent-sessions")]
    registry.set_state(session_id, SessionState::Listing);

    // Read module request with timeout
    line_buf.clear();
    let read_result = timeout(config.read_timeout, reader.read_line(&mut line_buf)).await;

    let module_request = match read_result {
        Ok(Ok(n)) if n > 0 => {
            bytes_received += n as u64;
            line_buf.trim().to_string()
        }
        Ok(Ok(_)) => String::new(),
        Ok(Err(e)) => return Err(AsyncDaemonError::Io(e)),
        Err(_) => return Err(AsyncDaemonError::Timeout(config.read_timeout)),
    };

    let module = if module_request.is_empty() || module_request == "#list" {
        None
    } else {
        Some(module_request.clone())
    };

    #[cfg(feature = "concurrent-sessions")]
    if let Some(ref m) = module {
        registry.set_module(session_id, m.clone());
        pool.set_module(conn_id, m.clone());
    }

    // For now, send an error response (full module handling would go here)
    let error_msg = "@ERROR: daemon functionality limited in async mode\n";
    writer.write_all(error_msg.as_bytes()).await?;
    writer.write_all(b"@RSYNCD: EXIT\n").await?;
    writer.flush().await?;
    bytes_sent += error_msg.len() as u64 + 14;

    #[cfg(feature = "concurrent-sessions")]
    {
        registry.set_state(session_id, SessionState::Completed);
        registry.add_bytes(session_id, bytes_received, bytes_sent);
        pool.add_bytes(conn_id, bytes_received, bytes_sent);
    }

    Ok(SessionResult {
        peer_addr,
        bytes_received,
        bytes_sent,
        module,
        success: true,
    })
}

/// Async rate limiter for session I/O.
#[derive(Debug)]
pub struct AsyncRateLimiter {
    bytes_per_second: u64,
    tokens: f64,
    last_update: std::time::Instant,
}

impl AsyncRateLimiter {
    /// Creates a new rate limiter with the specified bytes per second limit.
    #[must_use]
    pub fn new(bytes_per_second: u64) -> Self {
        Self {
            bytes_per_second,
            tokens: bytes_per_second as f64,
            last_update: std::time::Instant::now(),
        }
    }

    /// Acquires permission to transfer the specified number of bytes.
    ///
    /// If rate limiting is required, this method will sleep.
    pub async fn acquire(&mut self, bytes: usize) {
        let bytes = bytes as f64;

        // Replenish tokens based on elapsed time
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.bytes_per_second as f64)
            .min(self.bytes_per_second as f64 * 2.0); // Allow burst up to 2x
        self.last_update = now;

        // Check if we have enough tokens
        if self.tokens >= bytes {
            self.tokens -= bytes;
        } else {
            // Need to wait
            let needed = bytes - self.tokens;
            let wait_secs = needed / self.bytes_per_second as f64;
            tokio::time::sleep(Duration::from_secs_f64(wait_secs)).await;
            self.tokens = 0.0;
            self.last_update = std::time::Instant::now();
        }
    }

    /// Returns the current available bytes (tokens).
    #[must_use]
    pub fn available_bytes(&self) -> u64 {
        self.tokens as u64
    }
}

/// Per-IP connection limiter for async sessions.
#[derive(Debug)]
pub struct AsyncIpLimiter {
    #[cfg(feature = "concurrent-sessions")]
    pool: Arc<ConnectionPool>,
    max_per_ip: NonZeroU32,
}

impl AsyncIpLimiter {
    /// Creates a new IP limiter.
    #[cfg(feature = "concurrent-sessions")]
    #[must_use]
    pub fn new(pool: Arc<ConnectionPool>, max_per_ip: NonZeroU32) -> Self {
        Self { pool, max_per_ip }
    }

    /// Creates a new IP limiter without connection pool tracking.
    #[cfg(not(feature = "concurrent-sessions"))]
    #[must_use]
    pub fn new(max_per_ip: NonZeroU32) -> Self {
        Self { max_per_ip }
    }

    /// Checks if a new connection from the given IP would exceed the limit.
    #[cfg(feature = "concurrent-sessions")]
    #[must_use]
    pub fn would_exceed_limit(&self, ip: &std::net::IpAddr) -> bool {
        self.pool.would_exceed_ip_limit(ip, self.max_per_ip)
    }

    /// Checks if a new connection would exceed the limit (always false without tracking).
    #[cfg(not(feature = "concurrent-sessions"))]
    #[must_use]
    pub fn would_exceed_limit(&self, _ip: &std::net::IpAddr) -> bool {
        false
    }

    /// Returns the maximum connections per IP.
    #[must_use]
    pub fn max_per_ip(&self) -> NonZeroU32 {
        self.max_per_ip
    }
}

#[cfg(test)]
mod tests {
    //! Note: Async tests use manual `tokio::runtime::Runtime::new().unwrap().block_on()`
    //! instead of `#[tokio::test]` because the macro expands to use `core::future`
    //! which conflicts with our local 'core' crate that shadows the built-in core crate.

    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_listener_config_defaults() {
        let config = ListenerConfig::new();
        assert_eq!(config.max_connections, DEFAULT_MAX_CONNECTIONS);
        assert_eq!(
            config.connection_timeout,
            Duration::from_secs(DEFAULT_CONNECTION_TIMEOUT)
        );
        assert!(config.tcp_keepalive);
    }

    #[test]
    fn test_listener_config_builder() {
        let addr: SocketAddr = "127.0.0.1:8873".parse().unwrap();
        let config = ListenerConfig::new()
            .bind_address(addr)
            .max_connections(50)
            .connection_timeout(Duration::from_secs(30))
            .tcp_keepalive(false);

        assert_eq!(config.bind_address, addr);
        assert_eq!(config.max_connections, 50);
        assert_eq!(config.connection_timeout, Duration::from_secs(30));
        assert!(!config.tcp_keepalive);
    }

    #[test]
    fn test_listener_config_min_connections() {
        let config = ListenerConfig::new().max_connections(0);
        assert_eq!(config.max_connections, 1); // Should be clamped to 1
    }

    #[test]
    fn test_async_rate_limiter() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut limiter = AsyncRateLimiter::new(1000);

            // First acquire should be instant
            let start = std::time::Instant::now();
            limiter.acquire(500).await;
            assert!(start.elapsed() < Duration::from_millis(50));

            // Second acquire should also be fast (we have tokens)
            limiter.acquire(500).await;

            // This should deplete tokens
            assert!(limiter.available_bytes() < 100);
        });
    }

    #[test]
    fn test_async_listener_bind() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = ListenerConfig::new().bind_address("127.0.0.1:0".parse().unwrap());

            let listener = AsyncDaemonListener::bind(config).await.unwrap();
            let addr = listener.local_addr().unwrap();

            assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
            assert_ne!(addr.port(), 0);
        });
    }

    #[test]
    fn test_shutdown_signal() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = ListenerConfig::new().bind_address("127.0.0.1:0".parse().unwrap());

            let listener = AsyncDaemonListener::bind(config).await.unwrap();
            let shutdown = listener.shutdown_signal();

            // Subscribe a receiver so send succeeds
            let mut rx = shutdown.subscribe();

            // Should be able to send shutdown
            assert!(shutdown.send(()).is_ok());

            // Receiver should get the message
            assert!(rx.recv().await.is_ok());
        });
    }

    #[test]
    fn test_session_result() {
        let result = SessionResult {
            peer_addr: "127.0.0.1:12345".parse().unwrap(),
            bytes_received: 100,
            bytes_sent: 200,
            module: Some("test".to_string()),
            success: true,
        };

        assert_eq!(result.bytes_received, 100);
        assert_eq!(result.bytes_sent, 200);
        assert_eq!(result.module, Some("test".to_string()));
        assert!(result.success);
    }

    #[cfg(feature = "concurrent-sessions")]
    #[test]
    fn test_listener_with_registry() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = ListenerConfig::new().bind_address("127.0.0.1:0".parse().unwrap());

            let listener = AsyncDaemonListener::bind(config).await.unwrap();

            assert_eq!(listener.session_registry().count(), 0);
            assert_eq!(listener.connection_pool().count(), 0);
        });
    }
}
