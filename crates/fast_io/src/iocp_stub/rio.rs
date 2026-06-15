//! Cross-platform stub mirroring [`crate::iocp::rio`] (NET-RIO.1-3).
//!
//! Compiled on every non-Windows target and on Windows builds without the
//! `iocp` feature. All constructors return [`std::io::ErrorKind::Unsupported`]
//! or the equivalent `None` so callers behind a runtime [`try_init_rio`]
//! check transparently fall back to standard socket I/O.
//!
//! The public surface here is a strict subset of the Windows implementation
//! - just enough to let cross-platform code name the types and dispatch
//! against the env-var-driven mode toggle without `#[cfg(windows)]`
//! plumbing at every call site.

#![allow(dead_code)]

use std::io;

/// Default registered-buffer-pool size mirrored from the Windows backend.
pub const DEFAULT_RIO_POOL_BYTES: usize = 1024 * 1024;

/// Per-buffer slot size mirrored from the Windows backend.
pub const DEFAULT_RIO_SLOT_BYTES: usize = 32 * 1024;

/// Environment variable that selects the RIO mode at process start.
pub const RIO_ENV_VAR: &str = "OC_RSYNC_WINDOWS_RIO";

/// Mirrors [`crate::iocp::rio::RioMode`] so cross-platform callers can
/// thread a single type through their config plumbing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RioMode {
    /// RIO disabled.
    Off,
    /// Attempt RIO; transparent fallback to standard sockets.
    Auto,
    /// Require RIO (never satisfied on this platform).
    On,
}

impl Default for RioMode {
    fn default() -> Self {
        Self::Off
    }
}

/// Parses the `OC_RSYNC_WINDOWS_RIO` env var into a [`RioMode`].
///
/// The stub honours the env var so behavior-only tests can validate parsing
/// on every platform. The Windows-only side effect (actually engaging the
/// RIO path) is gated by [`try_init_rio`] returning `Ok(None)` on this
/// platform.
#[must_use]
pub fn rio_enabled_from_env() -> RioMode {
    parse_rio_env(std::env::var(RIO_ENV_VAR).ok().as_deref())
}

/// Pure parser, identical to the Windows backend for cross-platform tests.
#[doc(hidden)]
#[must_use]
pub fn parse_rio_env(value: Option<&str>) -> RioMode {
    match value.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("on") => RioMode::On,
        Some(s) if s.eq_ignore_ascii_case("auto") => RioMode::Auto,
        _ => RioMode::Off,
    }
}

/// Stub RIO extension function table. Construction always fails because
/// [`try_init_rio`] returns `Ok(None)` on this platform.
#[derive(Clone, Copy, Debug)]
pub struct RioFunctions {
    _private: (),
}

impl RioFunctions {
    /// Returns `false` on this platform - RIO is never available off Windows.
    #[must_use]
    pub fn is_available(&self) -> bool {
        false
    }
}

/// Always returns `Ok(None)` on this platform: RIO is Windows-only.
#[must_use]
pub fn try_init_rio() -> io::Result<Option<RioFunctions>> {
    Ok(None)
}

/// Stub registered buffer pool. Construction always fails with
/// [`std::io::ErrorKind::Unsupported`].
pub struct RioBufferPool {
    _private: (),
}

impl RioBufferPool {
    /// Returns `Unsupported` on this platform.
    pub fn new(_rio: &RioFunctions) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "RIO buffer pool is Windows-only",
        ))
    }

    /// Returns `Unsupported` on this platform.
    pub fn with_capacity(
        _rio: &RioFunctions,
        _total_bytes: usize,
        _slot_bytes: usize,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "RIO buffer pool is Windows-only",
        ))
    }

    /// Slot size is zero on this platform (no allocation has occurred).
    #[must_use]
    pub fn slot_size(&self) -> u32 {
        0
    }

    /// Slot count is zero on this platform.
    #[must_use]
    pub fn slot_count(&self) -> u32 {
        0
    }

    /// Available slot count is zero on this platform.
    #[must_use]
    pub fn available_slots(&self) -> usize {
        0
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn acquire(&self) -> Option<RegisteredBuffer> {
        None
    }
}

/// Stub registered buffer handle. Cannot be constructed on this platform.
pub struct RegisteredBuffer {
    _private: (),
}

/// Stub completion queue. Construction always fails with `Unsupported`.
pub struct RioCompletionQueue {
    _private: (),
}

impl RioCompletionQueue {
    /// Returns `Unsupported` on this platform.
    pub fn new(_rio: &RioFunctions, _depth: u32) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "RIO completion queue is Windows-only",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_default_is_off() {
        assert_eq!(parse_rio_env(None), RioMode::Off);
        assert_eq!(parse_rio_env(Some("")), RioMode::Off);
        assert_eq!(parse_rio_env(Some("off")), RioMode::Off);
    }

    #[test]
    fn env_var_parses_auto_case_insensitive() {
        assert_eq!(parse_rio_env(Some("auto")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("AUTO")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("Auto")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("aUtO")), RioMode::Auto);
    }

    #[test]
    fn env_var_parses_on_case_insensitive() {
        assert_eq!(parse_rio_env(Some("on")), RioMode::On);
        assert_eq!(parse_rio_env(Some("ON")), RioMode::On);
        assert_eq!(parse_rio_env(Some("On")), RioMode::On);
    }

    #[test]
    fn env_var_trims_whitespace() {
        assert_eq!(parse_rio_env(Some("  auto  ")), RioMode::Auto);
        assert_eq!(parse_rio_env(Some("\ton\n")), RioMode::On);
    }

    #[test]
    fn env_var_unknown_values_fall_back_to_off() {
        assert_eq!(parse_rio_env(Some("yes")), RioMode::Off);
        assert_eq!(parse_rio_env(Some("1")), RioMode::Off);
        assert_eq!(parse_rio_env(Some("true")), RioMode::Off);
    }

    #[test]
    fn rio_mode_default_is_off() {
        assert_eq!(RioMode::default(), RioMode::Off);
    }

    #[test]
    fn try_init_returns_none_on_stub_platform() {
        let result = try_init_rio().expect("stub init must not fail");
        assert!(result.is_none(), "RIO unavailable on non-Windows targets");
    }

    #[test]
    fn buffer_pool_construction_is_unsupported() {
        let stub_rio = RioFunctions { _private: () };
        let err = RioBufferPool::new(&stub_rio).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        let err = RioBufferPool::with_capacity(&stub_rio, 4096, 1024).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn completion_queue_construction_is_unsupported() {
        let stub_rio = RioFunctions { _private: () };
        let err = RioCompletionQueue::new(&stub_rio, 64).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
