//! Stub IOCP completion-port pump.
//!
//! Mirrors the public surface of [`crate::iocp::pump`] so cross-platform
//! callers can name [`CompletionPump`], [`CompletionHandler`], and
//! [`IocpPumpConfig`] behind a runtime IOCP availability check without
//! `#[cfg]` branching. Construction always fails with
//! [`io::ErrorKind::Unsupported`] on this platform.

use std::io;

/// Boxed completion-handler type mirroring the Windows pump API.
///
/// On non-Windows platforms the pump is never constructed, so the alias
/// exists only to keep downstream code that names the type compiling.
pub type CompletionHandler = Box<dyn FnOnce(io::Result<u32>) + Send + 'static>;

/// Configuration mirror for [`CompletionPump`] on non-Windows platforms.
#[derive(Debug, Clone, Default)]
pub struct IocpPumpConfig {
    /// Maximum concurrent worker threads (informational only on this platform).
    pub max_concurrent_threads: u32,
    /// Drain batch size (informational only on this platform).
    pub batch_size: usize,
}

/// Stub IOCP completion-port pump.
///
/// Construction always fails with [`io::ErrorKind::Unsupported`]. The type
/// exists so downstream callers can reference it from cross-platform code
/// behind a runtime check on [`is_iocp_available`](super::is_iocp_available).
#[derive(Debug)]
pub struct CompletionPump {
    _private: (),
}

impl CompletionPump {
    /// Returns `Unsupported` on this platform.
    pub fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP completion pump is not available on this platform",
        ))
    }

    /// Returns `Unsupported` on this platform.
    pub fn with_config(_config: IocpPumpConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP completion pump is not available on this platform",
        ))
    }

    /// Returns `false` on this platform; the pump is never running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        false
    }

    /// Always reports zero pending operations on this platform.
    #[must_use]
    pub fn pending_ops(&self) -> usize {
        0
    }

    /// Always returns `Ok(())` on this platform.
    pub fn shutdown(self) -> io::Result<()> {
        Ok(())
    }
}

/// Stub `oneshot_handler` matching the Windows API.
///
/// Returns a no-op handler and an empty receiver because the pump cannot be
/// constructed on this platform; the receiver will never produce a value.
#[must_use]
pub fn oneshot_handler() -> (
    CompletionHandler,
    std::sync::mpsc::Receiver<io::Result<u32>>,
) {
    let (_tx, rx) = std::sync::mpsc::channel::<io::Result<u32>>();
    let handler: CompletionHandler = Box::new(|_| {});
    (handler, rx)
}
