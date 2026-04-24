//! Tokio-based async session handling for the rsync daemon.
//!
//! Provides async alternatives to the synchronous session handling path,
//! available only when the `async` feature is enabled. Includes a TCP
//! listener with configurable connection limits, per-session timeout support,
//! `SessionRegistry` integration for concurrent session tracking, and
//! graceful shutdown via broadcast channels.
//!
//! # Submodules
//!
//! - [`listener`] - TCP listener setup and connection acceptance.
//! - [`session`] - Per-connection session handling, rate limiting, and IP limiting.
//! - [`shutdown`] - Error and shutdown types.
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

#![allow(dead_code)] // REASON: async daemon path not yet wired to production; types used in tests

mod listener;
mod session;
mod shutdown;

#[cfg(test)]
pub use listener::AsyncDaemonListener;

#[cfg(test)]
mod tests {
    //! Note: Async tests use manual `tokio::runtime::Runtime::new().unwrap().block_on()`
    //! instead of `#[tokio::test]` because the macro expands to use `core::future`
    //! which conflicts with our local 'core' crate that shadows the built-in core crate.

    use super::*;
    use listener::{DEFAULT_CONNECTION_TIMEOUT, DEFAULT_MAX_CONNECTIONS, ListenerConfig};
    use session::{AsyncRateLimiter, SessionResult};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

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
