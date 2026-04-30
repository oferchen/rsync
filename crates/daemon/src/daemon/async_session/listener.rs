//! TCP listener setup and connection acceptance for the async daemon.
//!
//! Contains [`ListenerConfig`] (builder-pattern configuration) and
//! [`AsyncDaemonListener`] which binds a socket, enforces connection
//! limits via a semaphore, and spawns per-connection handler tasks.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast};

#[cfg(feature = "concurrent-sessions")]
use super::super::session_registry::SessionRegistry;

#[cfg(feature = "concurrent-sessions")]
use super::super::connection_pool::ConnectionPool;

use super::session::{AsyncSession, handle_async_session};
use super::shutdown::AsyncDaemonError;

/// Default maximum number of concurrent connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 200;

/// Default connection timeout in seconds.
pub const DEFAULT_CONNECTION_TIMEOUT: u64 = 60;

/// Default read timeout for session I/O in seconds.
pub const DEFAULT_READ_TIMEOUT: u64 = 30;

/// Configuration for the async daemon listener.
#[derive(Debug, Clone)]
#[allow(dead_code)]
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

#[allow(dead_code)]
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

/// Async TCP listener for the rsync daemon.
#[allow(dead_code)]
pub struct AsyncDaemonListener {
    pub(super) listener: TcpListener,
    pub(super) config: ListenerConfig,
    pub(super) connection_semaphore: Arc<Semaphore>,
    pub(super) shutdown_tx: broadcast::Sender<()>,
    #[cfg(feature = "concurrent-sessions")]
    pub(super) session_registry: Arc<SessionRegistry>,
    #[cfg(feature = "concurrent-sessions")]
    pub(super) connection_pool: Arc<ConnectionPool>,
}

#[allow(dead_code)]
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

                    let permit = match self.connection_semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            // Connection limit reached - close immediately so the client sees
                            // EOF instead of being held in a half-open state.
                            drop(stream);
                            continue;
                        }
                    };

                    #[cfg(feature = "concurrent-sessions")]
                    let session_id = self.session_registry.register(peer_addr, None);
                    #[cfg(feature = "concurrent-sessions")]
                    let conn_id = self.connection_pool.register(peer_addr);

                    let config = self.config.clone();
                    #[cfg(feature = "concurrent-sessions")]
                    let registry = self.session_registry.clone();
                    #[cfg(feature = "concurrent-sessions")]
                    let pool = self.connection_pool.clone();

                    // Tokio captures panics in spawned tasks (returning Err(JoinError) from
                    // the JoinHandle). We log them explicitly so panics in one connection
                    // never tear down the daemon - matching upstream rsync's
                    // fork-per-connection isolation.
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

                        #[cfg(feature = "concurrent-sessions")]
                        {
                            registry.unregister(&session_id);
                            pool.unregister(&conn_id);
                        }

                        drop(permit);

                        if let Err(e) = result {
                            eprintln!(
                                "async session handler for {peer_addr} \
                                 failed: {e} [daemon={}]",
                                env!("CARGO_PKG_VERSION")
                            );
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
