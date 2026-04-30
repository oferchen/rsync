//! Per-connection session handling for the async daemon.
//!
//! Contains [`AsyncSession`] (the per-connection state), [`SessionResult`]
//! (transfer outcome), the core [`handle_async_session`] protocol handler,
//! [`AsyncRateLimiter`] (token-bucket I/O throttle), and [`AsyncIpLimiter`]
//! (per-IP connection cap).

use std::net::SocketAddr;
use std::num::NonZeroU32;
#[cfg(feature = "concurrent-sessions")]
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[cfg(feature = "concurrent-sessions")]
use super::super::session_registry::{SessionId, SessionRegistry, SessionState};

#[cfg(feature = "concurrent-sessions")]
use super::super::connection_pool::{ConnectionId, ConnectionPool};

use super::listener::ListenerConfig;
use super::shutdown::AsyncDaemonError;

/// An async daemon session.
pub struct AsyncSession {
    pub(super) stream: Option<TcpStream>,
    pub(super) peer_addr: SocketAddr,
    pub(super) config: ListenerConfig,
    #[cfg(feature = "concurrent-sessions")]
    pub(super) session_id: SessionId,
    #[cfg(feature = "concurrent-sessions")]
    pub(super) conn_id: ConnectionId,
    #[cfg(feature = "concurrent-sessions")]
    pub(super) registry: Arc<SessionRegistry>,
    #[cfg(feature = "concurrent-sessions")]
    pub(super) pool: Arc<ConnectionPool>,
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
#[allow(dead_code)]
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
pub(super) async fn handle_async_session(
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

    let greeting = format!("@RSYNCD: {}.0\n", 32);
    writer.write_all(greeting.as_bytes()).await?;
    writer.flush().await?;
    bytes_sent += greeting.len() as u64;

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

    let _client_version = if version_line.starts_with("@RSYNCD:") {
        version_line
            .strip_prefix("@RSYNCD:")
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(28.0) as u8
    } else {
        28
    };

    #[cfg(feature = "concurrent-sessions")]
    registry.set_state(session_id, SessionState::Listing);

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

    // Module handling lives on the synchronous daemon path; the async listener responds with
    // the upstream-compatible @ERROR + @RSYNCD: EXIT sequence so clients fail cleanly.
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

        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        // Cap accumulated tokens at 2x the per-second rate to allow short bursts without
        // unbounded credit accumulation across idle periods.
        self.tokens = (self.tokens + elapsed * self.bytes_per_second as f64)
            .min(self.bytes_per_second as f64 * 2.0);
        self.last_update = now;

        if self.tokens >= bytes {
            self.tokens -= bytes;
        } else {
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
