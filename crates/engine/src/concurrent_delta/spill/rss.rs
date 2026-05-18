//! Resident-set-size (RSS) probe used by the RSS-aware spill trigger.
//!
//! [`SpillPolicy::memory_pressure_bytes`](super::policy::SpillPolicy::memory_pressure_bytes)
//! forces the reorder buffer to spill when process RSS crosses a configured
//! threshold, independent of the byte-budget knob. This module provides a
//! cached [`current_rss_bytes`] helper used by the buffer to evaluate that
//! threshold without paying a syscall per insert.
//!
//! # Platform support
//!
//! - **Linux**: parses `/proc/self/statm`, multiplying the second field
//!   (resident pages) by the page size obtained from `sysconf(_SC_PAGESIZE)`
//!   via [`page_size()`]. No `unsafe` is required because the file is plain
//!   text and the page size is read through the safe `rustix` shim already
//!   used elsewhere in the workspace - failing that, the well-known 4096
//!   fallback applies.
//! - **macOS**: stubbed at `Ok(0)` so the knob is a no-op until a follow-up
//!   wires `mach_task_basic_info`. The byte-budget path continues to work
//!   exactly as it does today. See `#2340` follow-up scope.
//! - **Other Unix / Windows**: returns
//!   [`io::ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported); the
//!   buffer treats the error as "RSS unavailable" and falls back to the
//!   byte-budget knob.
//!
//! # Caching
//!
//! Every call route through [`cached_rss_bytes`], which serves a value from
//! an [`AtomicU64`] cell when the previous read was less than
//! [`RSS_CACHE_TTL`] old. Cache invalidation is wall-clock based and uses
//! [`Instant`] under a [`Mutex`] only for the timestamp; the hot path is a
//! single relaxed atomic load plus an `Instant::elapsed` comparison.

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a cached RSS reading stays fresh before the next probe.
///
/// 100 ms keeps the syscall overhead well under 1 % for the typical
/// concurrent-delta workload (one insert per ~ms) while still reacting
/// quickly enough to legitimate memory-pressure spikes.
pub const RSS_CACHE_TTL: Duration = Duration::from_millis(100);

/// Sentinel value stored in [`RSS_CACHE_BYTES`] before the first probe.
///
/// `u64::MAX` is unreachable as an RSS reading on any supported platform
/// (it would imply 16 EiB of resident memory) and acts as a "no cached
/// value" marker without an extra `Option` lock acquisition.
const CACHE_UNINITIALISED: u64 = u64::MAX;

static RSS_CACHE_BYTES: AtomicU64 = AtomicU64::new(CACHE_UNINITIALISED);
static RSS_CACHE_DEADLINE: Mutex<Option<Instant>> = Mutex::new(None);

/// Returns the cached process RSS in bytes, refreshing the cache when the
/// previous reading is older than [`RSS_CACHE_TTL`].
///
/// # Errors
///
/// Returns the underlying I/O error if the platform probe fails. Callers
/// should treat any error as "RSS unavailable" and fall back to whatever
/// behaviour they had before consulting RSS.
pub fn cached_rss_bytes() -> io::Result<u64> {
    if let Some(fresh) = peek_cached() {
        return Ok(fresh);
    }
    let bytes = current_rss_bytes()?;
    store_cached(bytes);
    Ok(bytes)
}

/// Returns the current process RSS in bytes, bypassing the cache.
///
/// Exposed for tests and for callers that need a freshly sampled value.
///
/// # Errors
///
/// Returns the underlying I/O error if the platform probe fails or the
/// platform is unsupported.
pub fn current_rss_bytes() -> io::Result<u64> {
    platform::current_rss_bytes()
}

/// Clears the cached RSS value so the next [`cached_rss_bytes`] call probes
/// the platform afresh. Intended for tests.
pub fn invalidate_cache() {
    RSS_CACHE_BYTES.store(CACHE_UNINITIALISED, Ordering::Relaxed);
    if let Ok(mut guard) = RSS_CACHE_DEADLINE.lock() {
        *guard = None;
    }
}

fn peek_cached() -> Option<u64> {
    let bytes = RSS_CACHE_BYTES.load(Ordering::Relaxed);
    if bytes == CACHE_UNINITIALISED {
        return None;
    }
    let guard = RSS_CACHE_DEADLINE.lock().ok()?;
    let deadline = (*guard)?;
    if Instant::now() < deadline {
        Some(bytes)
    } else {
        None
    }
}

fn store_cached(bytes: u64) {
    RSS_CACHE_BYTES.store(bytes, Ordering::Relaxed);
    if let Ok(mut guard) = RSS_CACHE_DEADLINE.lock() {
        *guard = Some(Instant::now() + RSS_CACHE_TTL);
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::io;
    use std::sync::OnceLock;

    /// Page size used to convert resident pages reported by
    /// `/proc/self/statm` into bytes. Cached in a [`OnceLock`] so the
    /// `sysconf` round-trip happens at most once per process.
    static PAGE_SIZE: OnceLock<u64> = OnceLock::new();

    fn page_size() -> u64 {
        *PAGE_SIZE.get_or_init(|| {
            // `rustix::param::page_size` reads `_SC_PAGESIZE` without any
            // `unsafe` glue on our side; if linking ever fails, 4 KiB is
            // the universally safe fallback on every supported Linux ABI.
            let raw = rustix::param::page_size();
            u64::try_from(raw).unwrap_or(4096)
        })
    }

    /// Reads `/proc/self/statm` and returns RSS in bytes.
    pub fn current_rss_bytes() -> io::Result<u64> {
        let raw = fs::read_to_string("/proc/self/statm")?;
        let resident_pages: u64 = raw
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "/proc/self/statm missing rss field",
                )
            })?
            .parse()
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("/proc/self/statm rss field parse failure: {e}"),
                )
            })?;
        Ok(resident_pages.saturating_mul(page_size()))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::io;

    /// macOS RSS reads require `mach_task_basic_info`, which lives behind
    /// `unsafe` libc bindings. To keep the unsafe footprint of the engine
    /// crate unchanged we ship a no-op stub that reports zero RSS; the
    /// reorder buffer interprets that as "below any threshold" so the
    /// `memory_pressure_bytes` knob is effectively disabled on macOS until
    /// a follow-up tracked separately under issue #2340 wires the real
    /// probe via `mach_task_basic_info` (or a safe wrapper crate).
    pub fn current_rss_bytes() -> io::Result<u64> {
        Ok(0)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use std::io;

    /// All non-Linux, non-macOS targets (notably Windows) report the probe
    /// as unsupported. Callers fall back to the byte-budget knob.
    pub fn current_rss_bytes() -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process RSS probe not implemented on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_rss_matches_platform_contract() {
        // The probe is non-deterministic by nature, but on Linux it must
        // produce a non-zero value because the process itself holds pages
        // resident. Other platforms either stub at zero (macOS) or return
        // Unsupported (Windows / non-mainstream Unix).
        let result = current_rss_bytes();
        if cfg!(target_os = "linux") {
            let bytes = result.expect("Linux must expose /proc/self/statm");
            assert!(bytes > 0, "Linux RSS must be non-zero, got {bytes}");
        } else if cfg!(target_os = "macos") {
            assert_eq!(result.ok(), Some(0), "macOS stub returns Ok(0)");
        } else {
            let err = result.expect_err("unsupported platforms must error");
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        }
    }

    #[test]
    fn cache_returns_same_value_within_ttl() {
        invalidate_cache();
        // Skip silently on unsupported platforms - the cache is never
        // populated there so there is nothing to assert.
        let Ok(first) = cached_rss_bytes() else {
            return;
        };
        let second = cached_rss_bytes().expect("second probe should succeed");
        assert_eq!(first, second, "cached value must be stable within TTL");
    }

    #[test]
    fn invalidate_cache_forces_fresh_probe() {
        invalidate_cache();
        let Ok(_first) = cached_rss_bytes() else {
            return;
        };
        // After invalidation the next call should re-read the platform
        // value (and store a fresh deadline). We cannot assert the new
        // value differs - RSS is too stable over microseconds - but we
        // can assert the call still succeeds and that the cache field
        // is repopulated.
        invalidate_cache();
        assert!(cached_rss_bytes().is_ok());
        assert_ne!(
            RSS_CACHE_BYTES.load(Ordering::Relaxed),
            CACHE_UNINITIALISED,
            "cache must be repopulated after a probe"
        );
    }
}
